//! Deterministic local discovery over a bounded catalog index.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, Weak};

use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    ContentBlock, IdGenerator, ToolCall, ToolDefinition, ToolMetadata,
    ToolOutputPolicy, ToolPromptContribution, ToolResult, ToolSelection,
};

use super::selection::ToolSelectionUpdate;
use crate::runtime::Session;

const DOCUMENT_BYTES: usize = 16 * 1024;

/// One bounded search document; executable schemas remain in the owning registry.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct ToolSearchDocument {
    name: String,
    #[builder(default)]
    remote_name: Option<String>,
    terms: BTreeMap<String, usize>,
    description: String,
}

/// Accumulates relevant catalog text without traversing arbitrary schema payloads.
#[derive(Default)]
struct DocumentText {
    text: String,
    visited: usize,
}

impl DocumentText {
    /// Appends a field at a UTF-8 boundary within the shared document budget.
    fn push(&mut self, text: &str) {
        if self.text.len() >= DOCUMENT_BYTES {
            return;
        }
        self.text.push(' ');
        let mut end = text
            .len()
            .min(DOCUMENT_BYTES.saturating_sub(self.text.len()));
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(text.get(..end).unwrap_or_default());
    }

    /// Reads parameter names and descriptions with bounded depth and work on empty schemas.
    fn schema(&mut self, schema: &serde_json::Value, depth: usize) {
        if depth > 8
            || self.text.len() >= DOCUMENT_BYTES
            || self.visited >= 4096
        {
            return;
        }
        self.visited += 1;
        if let Some(description) = schema
            .get("description")
            .and_then(serde_json::Value::as_str)
        {
            self.push(description);
        }
        if let Some(properties) = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            for (name, child) in properties {
                if self.text.len() >= DOCUMENT_BYTES || self.visited >= 4096 {
                    break;
                }
                self.push(name);
                self.schema(child, depth + 1);
            }
        }
        if let Some(items) = schema.get("items") {
            self.schema(items, depth + 1);
        }
        for key in ["anyOf", "allOf", "oneOf"] {
            if let Some(children) =
                schema.get(key).and_then(serde_json::Value::as_array)
            {
                for child in children {
                    if self.text.len() >= DOCUMENT_BYTES || self.visited >= 4096
                    {
                        break;
                    }
                    self.schema(child, depth + 1);
                }
            }
        }
    }
}

impl From<(&str, &dyn AgentTool)> for ToolSearchDocument {
    /// Indexes only requested search fields, never result data or output schemas.
    fn from((name, tool): (&str, &dyn AgentTool)) -> Self {
        let definition = tool.definition();
        let mut text = DocumentText::default();
        text.push(name);
        let remote_name =
            tool.metadata().mcp_ref.as_ref().and_then(|reference| {
                text.push(reference.server_id.as_str());
                text.push(&reference.remote_name);
                // Longer names cannot equal a valid query, so retain no unbounded duplicate.
                (reference.remote_name.chars().count() <= 256)
                    .then(|| reference.remote_name.clone())
            });
        text.push(&definition.description);
        text.schema(&definition.parameters, 0);
        let mut terms = BTreeMap::new();
        for term in ToolSearchIndex::tokens(&text.text) {
            *terms.entry(term).or_default() += 1;
        }
        Self::builder()
            .name(name.into())
            .remote_name(remote_name)
            .terms(terms)
            .description(definition.description.chars().take(160).collect())
            .build()
    }
}

/// Catalog-scoped BM25 statistics reused across selection-only updates.
#[derive(Default)]
pub(in crate::runtime) struct ToolSearchIndex {
    documents: Vec<ToolSearchDocument>,
    frequencies: BTreeMap<String, usize>,
    average_length: f64,
}

impl FromIterator<ToolSearchDocument> for ToolSearchIndex {
    /// Precomputes corpus counts once for this immutable catalog revision.
    fn from_iter<T: IntoIterator<Item = ToolSearchDocument>>(
        documents: T,
    ) -> Self {
        let documents: Vec<_> = documents.into_iter().collect();
        let mut frequencies = BTreeMap::new();
        let mut total = 0;
        for document in &documents {
            total += document.terms.values().sum::<usize>();
            for term in document.terms.keys() {
                *frequencies.entry(term.clone()).or_default() += 1;
            }
        }
        let average_length = total as f64 / documents.len().max(1) as f64;
        Self {
            documents,
            frequencies,
            average_length,
        }
    }
}

impl ToolSearchIndex {
    /// Tokenizes both documents and queries with Unicode case folding, camelCase and CJK bigrams.
    fn tokens(text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut word = String::new();
        let mut cjk = false;
        let flush = |word: &mut String, cjk: bool, tokens: &mut Vec<String>| {
            if word.is_empty() {
                return;
            }
            if cjk {
                let chars: Vec<_> = word.chars().collect();
                if chars.len() == 1 {
                    tokens.push(word.clone());
                } else {
                    tokens.extend(
                        chars.windows(2).map(|pair| pair.iter().collect()),
                    );
                }
            } else {
                tokens.push(word.to_lowercase());
            }
            word.clear();
        };
        let mut characters = text.chars().peekable();
        let mut previous = None::<char>;
        while let Some(character) = characters.next() {
            let is_cjk = matches!(character, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{323af}' | '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}');
            if !character.is_alphanumeric() {
                flush(&mut word, cjk, &mut tokens);
                previous = None;
                continue;
            }
            let camel_boundary = !is_cjk
                && character.is_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_lowercase()
                        || (previous.is_uppercase()
                            && characters
                                .peek()
                                .is_some_and(|next| next.is_lowercase()))
                });
            if is_cjk != cjk || camel_boundary {
                flush(&mut word, cjk, &mut tokens);
            }
            word.push(character);
            cjk = is_cjk;
            previous = Some(character);
        }
        flush(&mut word, cjk, &mut tokens);
        tokens
    }

    /// Exposes search only while at least one non-disabled Deferred candidate exists.
    pub(super) fn available(&self, selection: &ToolSelection) -> bool {
        self.documents
            .iter()
            .any(|document| !selection.disabled.contains(&document.name))
    }
}

/// Validated query shape with fixed product limits.
struct ToolSearchArguments {
    query: String,
    limit: Option<usize>,
}

impl TryFrom<&ToolCall> for ToolSearchArguments {
    type Error = ToolError;
    /// Validates Unicode scalar length and rejects fractional or out-of-range limits.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        let invalid = || {
            ToolError::InvalidArguments { tool: call.name.clone(), message: "query must have 1..=256 Unicode characters and limit must be an integer from 1 to 20".into() }
        };
        let object = call.arguments.as_object().ok_or_else(invalid)?;
        if object.keys().any(|key| key != "query" && key != "limit") {
            return Err(invalid());
        }
        let query = object
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?
            .trim();
        let limit = match object.get("limit") {
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(invalid)?,
            ),
            None => None,
        };
        if !(1..=256).contains(&query.chars().take(257).count())
            || !(1..=20).contains(&limit.unwrap_or(8))
        {
            return Err(invalid());
        }
        // Allocate only validated trimmed input, even if the request contains a huge whitespace prefix.
        Ok(Self {
            query: query.to_string(),
            limit,
        })
    }
}

/// Borrowed ranking context; loading is a separate, revalidated transaction.
struct ToolSearch<'a> {
    index: &'a ToolSearchIndex,
    selection: &'a ToolSelection,
    arguments: &'a ToolSearchArguments,
}

impl ToolSearch<'_> {
    /// Ranks exact identities first, then BM25, with deterministic lexical ties.
    fn ranked(&self) -> Vec<String> {
        let terms: BTreeSet<_> = ToolSearchIndex::tokens(&self.arguments.query)
            .into_iter()
            .collect();
        let count = self.index.documents.len() as f64;
        let mut matches = Vec::new();
        for document in &self.index.documents {
            if self.selection.disabled.contains(&document.name) {
                continue;
            }
            let exact = document.name == self.arguments.query
                || document.remote_name.as_deref()
                    == Some(self.arguments.query.as_str());
            let length = document.terms.values().sum::<usize>() as f64;
            let mut score = 0.0;
            for term in &terms {
                if let Some(frequency) = document.terms.get(term) {
                    let documents =
                        *self.index.frequencies.get(term).unwrap_or(&0) as f64;
                    let idf = (1.0
                        + (count - documents + 0.5) / (documents + 0.5))
                        .ln();
                    let tf = *frequency as f64;
                    score += idf * tf * 2.2
                        / (tf
                            + 1.2
                                * (0.25
                                    + 0.75 * length
                                        / self.index.average_length.max(1.0)));
                }
            }
            if exact || score > 0.0 {
                matches.push((document.name.as_str(), exact, score));
            }
        }
        matches.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| right.2.total_cmp(&left.2))
                .then_with(|| left.0.cmp(right.0))
        });
        matches
            .into_iter()
            .take(self.arguments.limit.unwrap_or(8))
            .map(|(name, _, _)| name.to_string())
            .collect()
    }
}

/// Session discovery tool; the weak owner prevents a registry/Session reference cycle.
pub(in crate::runtime) struct ToolSearchTool {
    session: Weak<Session>,
    ids: Arc<dyn IdGenerator>,
}

impl ToolSearchTool {
    /// Retains only dependencies needed for durable selection updates.
    pub(in crate::runtime) fn new(
        session: Weak<Session>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { session, ids }
    }
}

#[async_trait]
impl AgentTool for ToolSearchTool {
    /// Advertises local discovery without including the entire Deferred catalog.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "tool_search".into(), description: "Find and load deferred tools. Loaded tools become callable in the next model request, never in this same batch.".into(), parameters: serde_json::json!({"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["query"],"additionalProperties":false}) }
    }

    /// Bounds descriptions and load reports after result hooks.
    fn metadata(&self) -> &ToolMetadata {
        static METADATA: LazyLock<ToolMetadata> = LazyLock::new(|| {
            ToolMetadata::builder()
                .output_policy(ToolOutputPolicy::Bounded)
                .build()
        });
        &METADATA
    }

    /// Explains next-request activation without injecting Deferred names or schemas.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution { snippet: Some("Search for additional tools by name or purpose.".into()), guidelines: vec!["After tool_search loads a tool, call it in the next request. Do not call unloaded tools in the same batch as search.".into()] }
    }

    /// Rejects malformed search inputs before committing any selection.
    fn validate(&self, call: &ToolCall) -> Result<(), ToolError> {
        if call.name != "tool_search" {
            return Err(ToolError::NotFound(call.name.clone()));
        }
        ToolSearchArguments::try_from(call).map(|_| ())
    }

    /// Searches locally and reports only the candidates accepted by the atomic load operation.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let arguments = ToolSearchArguments::try_from(&call)?;
        let session = self.session.upgrade().ok_or(ToolError::Cancelled)?;
        let snapshot = session.tools.snapshot()?;
        let ranked = ToolSearch {
            index: &snapshot.search_index,
            selection: &snapshot.selection,
            arguments: &arguments,
        }
        .ranked();
        let report = ToolSelectionUpdate::new(
            &session.tools,
            &session.transcript,
            self.ids.as_ref(),
        )
        .load(&ranked)
        .map_err(|error| ToolError::Execution {
            tool: call.name.clone(),
            message: error.to_string(),
        })?;
        if !report.loaded.is_empty() {
            session
                .notify_mcp_change(protocol::McpSessionChange::Selection, None)
                .map_err(|error| ToolError::Execution {
                    tool: call.name.clone(),
                    message: error.to_string(),
                })?;
        }
        let available = snapshot.available_registry();
        let summaries: Vec<_> = report.loaded.iter().chain(&report.already_loaded).filter_map(|name| available.tool(name).map(|tool| {
            // Reuse the bounded catalog summary instead of cloning a complete schema per query.
            let description = snapshot.search_index.documents.iter().find(|document| document.name == *name).map(|document| document.description.as_str()).unwrap_or_default();
            serde_json::json!({"name":name,"server":tool.metadata().mcp_ref.as_ref().map(|reference| &reference.server_id),"description":description})
        })).collect();
        let text = serde_json::json!({"selection":report,"tools":summaries,"notice":"Loaded tools are available in the next model request."}).to_string();
        Ok(ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text { text }])
            .is_error(false)
            .build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compact tool fixture for document construction without a live Session.
    struct SearchTool {
        name: &'static str,
        description: &'static str,
        schema: serde_json::Value,
    }

    #[async_trait]
    impl AgentTool for SearchTool {
        /// Supplies complete input metadata for the production document mapper.
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.into(),
                description: self.description.into(),
                parameters: self.schema.clone(),
            }
        }
        /// Search tests must not execute a tool.
        async fn execute(
            &self,
            _call: ToolCall,
            _context: &ToolExecutionContext,
        ) -> Result<ToolResult, ToolError> {
            unreachable!("search never executes")
        }
    }

    /// Builds documents through the real mapper, not pre-tokenized test data.
    fn index(tools: &[SearchTool]) -> ToolSearchIndex {
        tools
            .iter()
            .map(|tool| {
                ToolSearchDocument::from((tool.name, tool as &dyn AgentTool))
            })
            .collect()
    }

    /// Exact identities outrank descriptions; equal BM25 matches use lexical order.
    #[test]
    fn exact_identity_cjk_and_ties_are_deterministic() {
        let index = index(&[
            SearchTool {
                name: "ticket",
                description: "other purpose",
                schema: serde_json::json!({}),
            },
            SearchTool {
                name: "b",
                description: "ticket 查询工单",
                schema: serde_json::json!({}),
            },
            SearchTool {
                name: "a",
                description: "ticket 查询工单",
                schema: serde_json::json!({}),
            },
        ]);
        let selection = ToolSelection::default();
        let arguments = ToolSearchArguments {
            query: "ticket".into(),
            limit: None,
        };
        assert_eq!(
            ToolSearch {
                index: &index,
                selection: &selection,
                arguments: &arguments
            }
            .ranked(),
            vec!["ticket", "a", "b"]
        );
        let arguments = ToolSearchArguments {
            query: "工单".into(),
            limit: None,
        };
        assert_eq!(
            ToolSearch {
                index: &index,
                selection: &selection,
                arguments: &arguments
            }
            .ranked(),
            vec!["a", "b"]
        );
        assert_eq!(
            ToolSearchIndex::tokens("listIssues HTTPRequest 猫 Äpfel"),
            vec!["list", "issues", "http", "request", "猫", "äpfel"]
        );
    }

    /// Query limits count Unicode scalars while schema indexing stops at its documented bounds.
    #[test]
    fn arguments_and_document_work_are_bounded() {
        for (query, limit, valid) in [
            ("中".repeat(256), serde_json::json!(20), true),
            ("中".repeat(257), serde_json::json!(8), false),
            (" ".into(), serde_json::json!(8), false),
            ("query".into(), serde_json::json!(1.5), false),
            ("query".into(), serde_json::json!(21), false),
        ] {
            let call = ToolCall::builder()
                .tool_call_id(
                    protocol::ToolCallId::try_from("search").expect("id"),
                )
                .name("tool_search".into())
                .arguments(serde_json::json!({"query":query,"limit":limit}))
                .build();
            match ToolSearchArguments::try_from(&call) {
                Ok(_) => assert!(valid),
                Err(_) => assert!(!valid),
            }
        }
        let mut text = DocumentText::default();
        text.push(&"中文".repeat(10_000));
        assert!(text.text.len() <= DOCUMENT_BYTES);
        let mut schema = serde_json::json!({"description":"too-deep-sentinel"});
        for _ in 0..9 {
            schema = serde_json::json!({"properties":{"nested":schema}});
        }
        let mut text = DocumentText::default();
        text.schema(&schema, 0);
        assert!(!text.text.contains("too-deep-sentinel"));
    }
}
