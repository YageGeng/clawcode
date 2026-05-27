//! ACP client-side filesystem request handlers for the local TUI.

use agent_client_protocol::schema::{
    ReadTextFileRequest, ReadTextFileResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};

/// Handles an ACP client-side request to read a UTF-8 text file.
pub(crate) async fn handle_read_text_file(
    request: ReadTextFileRequest,
) -> Result<ReadTextFileResponse, agent_client_protocol::Error> {
    tracing::trace!(
        "[fs] client fs request received, handle_read_text_file: path={}",
        request.path.display()
    );

    if !request.path.is_absolute() {
        return Err(agent_client_protocol::Error::invalid_params().data(
            format!("path must be absolute: {}", request.path.display()),
        ));
    }

    if request.line == Some(0) {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("line must be 1-based"));
    }

    let content =
        tokio::fs::read_to_string(&request.path)
            .await
            .map_err(|error| {
                agent_client_protocol::Error::internal_error().data(format!(
                    "failed to read {}: {error}",
                    request.path.display()
                ))
            })?;

    let start_line = request.line.unwrap_or(1) as usize;
    let limit = request.limit.map(|limit| limit as usize);

    // `split_inclusive` preserves original line endings in partial reads,
    // matching ACP's text-file response semantics better than `lines()`.
    let mut lines = content.split_inclusive('\n').skip(start_line - 1);
    let selected = match limit {
        Some(limit) => lines.by_ref().take(limit).collect::<String>(),
        None => lines.collect::<String>(),
    };

    Ok(ReadTextFileResponse::new(selected))
}

/// Handles an ACP client-side request to write exact UTF-8 text content.
pub(crate) async fn handle_write_text_file(
    request: WriteTextFileRequest,
) -> Result<WriteTextFileResponse, agent_client_protocol::Error> {
    tracing::trace!(
        "[fs] client fs request received, handle_write_text_file: path={}",
        request.path.display()
    );

    if !request.path.is_absolute() {
        return Err(agent_client_protocol::Error::invalid_params().data(
            format!("path must be absolute: {}", request.path.display()),
        ));
    }

    let parent = request.path.parent().ok_or_else(|| {
        agent_client_protocol::Error::invalid_params()
            .data(format!("path has no parent: {}", request.path.display()))
    })?;
    let parent_exists =
        tokio::fs::try_exists(parent).await.map_err(|error| {
            agent_client_protocol::Error::internal_error().data(format!(
                "failed to inspect {}: {error}",
                parent.display()
            ))
        })?;
    if !parent_exists {
        return Err(agent_client_protocol::Error::internal_error().data(
            format!("parent directory does not exist: {}", parent.display()),
        ));
    }

    tokio::fs::write(&request.path, request.content)
        .await
        .map_err(|error| {
            agent_client_protocol::Error::internal_error().data(format!(
                "failed to write {}: {error}",
                request.path.display()
            ))
        })?;

    Ok(WriteTextFileResponse::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::SessionId;

    #[tokio::test]
    async fn read_text_file_request_reads_requested_line_window() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "one\ntwo\nthree\nfour\n")
            .await
            .expect("sample file should be written");

        let response = handle_read_text_file(
            ReadTextFileRequest::new(SessionId::new("s1"), path)
                .line(2)
                .limit(2),
        )
        .await
        .expect("read request should succeed");

        assert_eq!(response.content, "two\nthree\n");
    }

    #[tokio::test]
    async fn write_text_file_request_writes_exact_content() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let path = dir.path().join("out.txt");

        handle_write_text_file(WriteTextFileRequest::new(
            SessionId::new("s1"),
            path.clone(),
            "hello\nworld\n",
        ))
        .await
        .expect("write request should succeed");

        assert_eq!(
            tokio::fs::read_to_string(path)
                .await
                .expect("written file should be readable"),
            "hello\nworld\n"
        );
    }
}
