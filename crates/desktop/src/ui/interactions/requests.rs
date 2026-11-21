//! Docked agent requests: plan approvals, permission prompts, and MCP
//! elicitations, with the form dialog an elicitation opens (spec §7.3).

use std::collections::{BTreeMap, BTreeSet};

use agent_client_protocol::v2::schema as wire;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::Radio;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, div, prelude::FluentBuilder as _, rems,
};
use protocol::ProductIdentity;

use crate::backend::{AgentRequest, RequestId};
use crate::ui::interactions::elicitation_form::{
    ElicitationForm, FieldError, FieldKind, FieldValue,
};
use crate::ui::session::SessionView;

/// The plan text of a plan-approval request: a `switch_mode` tool call's text content.
fn plan_text(request: &wire::RequestPermissionRequest) -> Option<String> {
    let Some(wire::RequestPermissionSubject::ToolCall(subject)) =
        &request.subject
    else {
        return None;
    };
    let call = &subject.tool_call;
    if call.kind.value() != Some(&wire::ToolKind::SwitchMode) {
        return None;
    }
    let text = call
        .content
        .value()
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            wire::ToolCallContent::Content(content) => match &content.content {
                wire::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    Some(text)
}

impl SessionView {
    /// Answers a permission request and drops its card.
    fn answer_permission(
        &mut self,
        id: RequestId,
        option: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(client) = &self.client
            && let Err(error) = client.answer_permission(id, option.as_deref())
        {
            tracing::warn!(
                "failed to answer permission request {}: {}",
                id.0,
                error
            );
            window
                .push_notification(Notification::error(error.to_string()), cx);
        }
        self.drop_request(id, cx);
    }

    /// Answers an elicitation and drops its card; answering twice is harmless.
    pub(in crate::ui) fn answer_elicitation(
        &mut self,
        id: RequestId,
        action: wire::ElicitationAction,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(client) = &self.client
            && let Err(error) = client.answer_elicitation(
                id,
                wire::CreateElicitationResponse::new(action),
            )
        {
            tracing::warn!("failed to answer elicitation {}: {}", id.0, error);
        }
        self.drop_request(id, cx);
    }

    /// Removes an answered request from the session's controls.
    fn drop_request(&mut self, id: RequestId, cx: &mut Context<'_, Self>) {
        if let Some(handle) = self.handle().cloned() {
            handle.update(cx, |handle, cx| {
                if handle.controls_mut().remove_request(id) {
                    cx.notify();
                }
            });
        }
        cx.notify();
    }

    /// Closes the form dialog once its request is answered or withdrawn.
    pub(in crate::ui) fn close_withdrawn_form(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(open) = self.form_request else {
            return;
        };
        let pending = self.handle().is_some_and(|handle| {
            handle
                .read(cx)
                .projection()
                .controls()
                .requests()
                .iter()
                .any(|request| request.id() == open)
        });
        if !pending {
            self.form_request = None;
            window.close_dialog(cx);
        }
    }

    /// Opens the form of an elicitation; an unrenderable schema is cancelled with a notice.
    fn open_form(
        &mut self,
        id: RequestId,
        request: &wire::CreateElicitationRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let wire::ElicitationMode::Form(form) = &request.mode else {
            return;
        };
        let Ok(form) = ElicitationForm::try_from(&form.requested_schema) else {
            window.push_notification(
                Notification::warning(
                    rust_i18n::t!("Request.form_unsupported").into_owned(),
                ),
                cx,
            );
            self.answer_elicitation(id, wire::ElicitationAction::Cancel, cx);
            return;
        };
        let view = cx.entity().downgrade();
        let dialog = cx.new(|cx| {
            ElicitationDialog::new(id, form, view.clone(), window, cx)
        });
        self.form_request = Some(id);
        let title = server_name(request);
        window.open_dialog(cx, move |modal, _, cx| {
            let primary_text = cx.theme().primary_foreground;
            let (submit, decline, cancel) =
                (dialog.clone(), dialog.clone(), dialog.clone());
            let closing = view.clone();
            modal
                .title(Label::new(
                    rust_i18n::t!(
                        "Request.needs_input",
                        server = title.clone()
                    )
                    .into_owned(),
                ))
                .child(dialog.clone())
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("form-cancel")
                                .ghost()
                                .small()
                                .child(
                                    Label::new(
                                        rust_i18n::t!("Request.cancel")
                                            .into_owned(),
                                    )
                                    .text_xs(),
                                )
                                .on_click(move |_, window, cx| {
                                    cancel.update(cx, |dialog, cx| {
                                        dialog.finish(
                                            wire::ElicitationAction::Cancel,
                                            window,
                                            cx,
                                        )
                                    })
                                }),
                        )
                        .child(
                            Button::new("form-decline")
                                .ghost()
                                .small()
                                .child(
                                    Label::new(
                                        rust_i18n::t!("Request.decline")
                                            .into_owned(),
                                    )
                                    .text_xs(),
                                )
                                .on_click(move |_, window, cx| {
                                    decline.update(cx, |dialog, cx| {
                                        dialog.finish(
                                            wire::ElicitationAction::Decline,
                                            window,
                                            cx,
                                        )
                                    })
                                }),
                        )
                        .child(
                            Button::new("form-submit")
                                .primary()
                                .small()
                                .child(
                                    Label::new(
                                        rust_i18n::t!("Request.submit")
                                            .into_owned(),
                                    )
                                    .text_xs()
                                    .text_color(primary_text),
                                )
                                .on_click(move |_, window, cx| {
                                    submit.update(cx, |dialog, cx| {
                                        dialog.submit(window, cx)
                                    })
                                }),
                        ),
                )
                // Escape or the close button is a cancel; answering twice is harmless.
                .on_close(move |_, _, cx| {
                    let _ = closing.update(cx, |this, cx| {
                        this.form_request = None;
                        this.answer_elicitation(
                            id,
                            wire::ElicitationAction::Cancel,
                            cx,
                        );
                    });
                })
        });
    }

    /// Cards of the session's pending requests; `None` when there are none.
    pub(in crate::ui) fn render_requests(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let requests = self
            .handle()?
            .read(cx)
            .projection()
            .controls()
            .requests()
            .to_vec();
        if requests.is_empty() {
            return None;
        }
        let cards: Vec<AnyElement> = requests
            .into_iter()
            .map(|request| self.render_request(request, cx))
            .collect();
        Some(v_flex().w_full().children(cards).into_any_element())
    }

    /// One request card: a plan approval, a permission prompt, or an elicitation.
    fn render_request(
        &self,
        request: AgentRequest,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let id = request.id();
        let card = v_flex()
            .id(SharedString::from(format!("request:{}", id.0)))
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border);
        match request {
            AgentRequest::Permission { id, request } => {
                match plan_text(&request) {
                    Some(plan) => {
                        self.render_plan_approval(card, id, &request, plan, cx)
                    }
                    None => self.render_permission(card, id, &request, cx),
                }
            }
            AgentRequest::Elicitation { id, request } => {
                self.render_elicitation(card, id, &request, cx)
            }
        }
    }

    /// A request card's button; `primary` carries the text color of the card's main action.
    fn request_button(
        key: &str,
        label: String,
        primary: Option<Hsla>,
    ) -> Button {
        let button = Button::new(SharedString::from(key.to_owned())).small();
        // gpui-kit `Label` paints `foreground` unless it is given a color,
        // which is invisible on a `primary` fill.
        match primary {
            Some(text) => button
                .primary()
                .child(Label::new(label).text_xs().text_color(text)),
            None => button.ghost().child(Label::new(label).text_xs()),
        }
    }

    /// The id of the request's first option of `kind`, or `fallback` when it offers none.
    fn option_id(
        request: &wire::RequestPermissionRequest,
        kind: wire::PermissionOptionKind,
        fallback: &str,
    ) -> String {
        request
            .options
            .iter()
            .find(|option| option.kind == kind)
            .map(|option| option.option_id.to_string())
            .unwrap_or_else(|| fallback.to_owned())
    }

    /// A plan-mode plan waiting for approval: the plan, then keep planning or approve.
    fn render_plan_approval(
        &self,
        card: Stateful<Div>,
        id: RequestId,
        request: &wire::RequestPermissionRequest,
        plan: String,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (reject, approve) = (
            Self::option_id(
                request,
                wire::PermissionOptionKind::RejectOnce,
                "keep_planning",
            ),
            Self::option_id(
                request,
                wire::PermissionOptionKind::AllowOnce,
                "approve",
            ),
        );
        let primary = cx.theme().primary_foreground;
        let button = |key: &str, label: String, main: bool| {
            Self::request_button(key, label, main.then_some(primary))
        };
        card.child(
            Label::new(rust_i18n::t!("Request.plan_title").into_owned())
                .text_sm(),
        )
        .child(
            div()
                .id(SharedString::from(format!("request-plan:{}", id.0)))
                .max_h(rems(20.))
                .overflow_y_scroll()
                .child(TextView::markdown(
                    SharedString::from(format!("request-plan-text:{}", id.0)),
                    SharedString::from(plan),
                )),
        )
        .child(
            h_flex()
                .gap_1()
                .justify_end()
                .child(
                    button(
                        "request-reject",
                        rust_i18n::t!("Request.keep_planning").into_owned(),
                        false,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.answer_permission(
                                id,
                                Some(reject.clone()),
                                window,
                                cx,
                            )
                        },
                    )),
                )
                .child(
                    button(
                        "request-approve",
                        rust_i18n::t!("Request.approve").into_owned(),
                        true,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.answer_permission(
                                id,
                                Some(approve.clone()),
                                window,
                                cx,
                            )
                        },
                    )),
                ),
        )
        .into_any_element()
    }

    /// A generic permission prompt: its title, description, and one button per option.
    fn render_permission(
        &self,
        card: Stateful<Div>,
        id: RequestId,
        request: &wire::RequestPermissionRequest,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let primary = cx.theme().primary_foreground;
        let button = |key: &str, label: String, main: bool| {
            Self::request_button(key, label, main.then_some(primary))
        };
        // Reject options first, as the WebUI orders them.
        let mut options = request.options.clone();
        options.sort_by_key(|option| {
            !matches!(
                option.kind,
                wire::PermissionOptionKind::RejectOnce
                    | wire::PermissionOptionKind::RejectAlways
            )
        });
        let buttons: Vec<AnyElement> = options
            .into_iter()
            .enumerate()
            .map(|(n, option)| {
                let option_id = option.option_id.to_string();
                button(
                    &format!("request-option-{n}"),
                    option.name.clone(),
                    n + 1 == request.options.len(),
                )
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.answer_permission(
                            id,
                            Some(option_id.clone()),
                            window,
                            cx,
                        )
                    },
                ))
                .into_any_element()
            })
            .collect();
        card.child(Label::new(request.title.clone()).text_sm())
            .when_some(request.description.clone(), |this, description| {
                this.child(
                    Label::new(description)
                        .text_xs()
                        .text_color(theme.muted_foreground),
                )
            })
            .child(h_flex().gap_1().justify_end().children(buttons))
            .into_any_element()
    }

    /// An MCP elicitation: a form to fill in, or a page to open.
    fn render_elicitation(
        &self,
        card: Stateful<Div>,
        id: RequestId,
        request: &wire::CreateElicitationRequest,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let primary = cx.theme().primary_foreground;
        let button = |key: &str, label: String, main: bool| {
            Self::request_button(key, label, main.then_some(primary))
        };
        let server = server_name(request);
        let url = match &request.mode {
            wire::ElicitationMode::Url(url) => Some(url.url.clone()),
            _ => None,
        };
        let host = url.as_deref().map(|url| {
            url.split("://")
                .nth(1)
                .unwrap_or(url)
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned()
        });
        let proceed = match url {
            Some(url) => button(
                "request-open",
                rust_i18n::t!("Request.open_continue").into_owned(),
                true,
            )
            .on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    cx.open_url(&url);
                    this.answer_elicitation(
                        id,
                        wire::ElicitationAction::Accept(
                            wire::ElicitationAcceptAction::new(),
                        ),
                        cx,
                    );
                },
            )),
            None => {
                let request = request.clone();
                button(
                    "request-fill",
                    rust_i18n::t!("Request.fill").into_owned(),
                    true,
                )
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.open_form(id, &request, window, cx)
                    },
                ))
            }
        };
        card.child(
            Label::new(
                rust_i18n::t!("Request.needs_input", server = server)
                    .into_owned(),
            )
            .text_sm(),
        )
        .child(
            Label::new(request.message.clone())
                .text_xs()
                .text_color(theme.muted_foreground),
        )
        .when_some(host, |this, host| {
            this.child(
                Label::new(
                    rust_i18n::t!("Request.will_open", host = host)
                        .into_owned(),
                )
                .text_xs()
                .text_color(theme.muted_foreground),
            )
        })
        .child(
            h_flex()
                .gap_1()
                .justify_end()
                .child(
                    button(
                        "request-cancel",
                        rust_i18n::t!("Request.cancel").into_owned(),
                        false,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.answer_elicitation(
                                id,
                                wire::ElicitationAction::Cancel,
                                cx,
                            )
                        },
                    )),
                )
                .child(
                    button(
                        "request-decline",
                        rust_i18n::t!("Request.decline").into_owned(),
                        false,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.answer_elicitation(
                                id,
                                wire::ElicitationAction::Decline,
                                cx,
                            )
                        },
                    )),
                )
                .child(proceed),
        )
        .into_any_element()
    }
}

/// The MCP server an elicitation comes from, or a generic name.
fn server_name(request: &wire::CreateElicitationRequest) -> String {
    request
        .meta
        .as_ref()
        .and_then(|meta| meta.get(ProductIdentity::ACP_NAMESPACE))
        .and_then(|product| product.get("mcpServerId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("MCP")
        .to_owned()
}

/// The form an elicitation asks the user to fill in, shown in a dialog.
#[derive(typed_builder::TypedBuilder)]
pub struct ElicitationDialog {
    request: RequestId,
    form: ElicitationForm,
    /// Text inputs of text, number, and choice-free fields.
    inputs: BTreeMap<String, Entity<InputState>>,
    /// Checkbox values.
    #[builder(default)]
    toggles: BTreeMap<String, bool>,
    /// Single choices.
    #[builder(default)]
    choices: BTreeMap<String, String>,
    /// Multiple choices.
    #[builder(default)]
    picks: BTreeMap<String, BTreeSet<String>>,
    /// Validation errors of the last submit.
    #[builder(default)]
    errors: BTreeMap<String, FieldError>,
    view: WeakEntity<SessionView>,
}

impl ElicitationDialog {
    /// Builds the inputs of every field.
    fn new(
        request: RequestId,
        form: ElicitationForm,
        view: WeakEntity<SessionView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let inputs = form
            .fields()
            .iter()
            .filter(|field| {
                matches!(
                    field.kind,
                    FieldKind::Text { .. } | FieldKind::Number { .. }
                )
            })
            .map(|field| {
                (field.name.clone(), cx.new(|cx| InputState::new(window, cx)))
            })
            .collect();
        ElicitationDialog::builder()
            .request(request)
            .form(form)
            .inputs(inputs)
            .view(view)
            .build()
    }

    /// The values entered so far, per field.
    fn values(&self, cx: &App) -> BTreeMap<String, FieldValue> {
        let mut values = BTreeMap::new();
        for (name, input) in &self.inputs {
            values.insert(
                name.clone(),
                FieldValue::Text(input.read(cx).value().to_string()),
            );
        }
        for (name, on) in &self.toggles {
            values.insert(name.clone(), FieldValue::Bool(*on));
        }
        for (name, choice) in &self.choices {
            values.insert(name.clone(), FieldValue::Text(choice.clone()));
        }
        for (name, picks) in &self.picks {
            values.insert(
                name.clone(),
                FieldValue::Many(picks.iter().cloned().collect()),
            );
        }
        values
    }

    /// Validates and accepts, or shows the errors and keeps the dialog open.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.form.validate(&self.values(cx)) {
            Ok(content) => {
                let accept =
                    wire::ElicitationAcceptAction::new().content(content);
                self.finish(
                    wire::ElicitationAction::Accept(accept),
                    window,
                    cx,
                );
            }
            Err(errors) => {
                self.errors = errors;
                cx.notify();
            }
        }
    }

    /// Answers the request and closes the dialog.
    fn finish(
        &mut self,
        action: wire::ElicitationAction,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let request = self.request;
        let _ = self.view.update(cx, |view, cx| {
            view.form_request = None;
            view.answer_elicitation(request, action, cx);
        });
        window.close_dialog(cx);
    }
}

impl Render for ElicitationDialog {
    /// Every field with its label, input, description, and error.
    fn render(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let fields: Vec<AnyElement> = self
            .form
            .fields()
            .to_vec()
            .into_iter()
            .map(|field| {
                let name = field.name.clone();
                let label = if field.required {
                    format!("{} *", field.label)
                } else {
                    field.label.clone()
                };
                let input: AnyElement = match &field.kind {
                    FieldKind::Text { .. } | FieldKind::Number { .. } => {
                        match self.inputs.get(&name) {
                            Some(input) => {
                                Input::new(input).small().into_any_element()
                            }
                            None => div().into_any_element(),
                        }
                    }
                    FieldKind::Bool => {
                        let on =
                            self.toggles.get(&name).copied().unwrap_or(false);
                        let key = name.clone();
                        Checkbox::new(SharedString::from(format!(
                            "field-{name}"
                        )))
                        .checked(on)
                        .on_click(cx.listener(
                            move |this, checked: &bool, _, cx| {
                                this.toggles.insert(key.clone(), *checked);
                                cx.notify();
                            },
                        ))
                        .into_any_element()
                    }
                    FieldKind::Choice { options } => {
                        let selected = self.choices.get(&name).cloned();
                        v_flex()
                            .gap_1()
                            .children(options.iter().enumerate().map(
                                |(n, (value, title))| {
                                    let (key, value) =
                                        (name.clone(), value.clone());
                                    Radio::new(SharedString::from(format!(
                                        "field-{name}-{n}"
                                    )))
                                    .label(title.clone())
                                    .checked(
                                        selected.as_deref()
                                            == Some(value.as_str()),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &bool, _, cx| {
                                            this.choices.insert(
                                                key.clone(),
                                                value.clone(),
                                            );
                                            cx.notify();
                                        },
                                    ))
                                },
                            ))
                            .into_any_element()
                    }
                    FieldKind::MultiChoice { options, .. } => {
                        let picked =
                            self.picks.get(&name).cloned().unwrap_or_default();
                        v_flex()
                            .gap_1()
                            .children(options.iter().enumerate().map(
                                |(n, (value, title))| {
                                    let (key, value) =
                                        (name.clone(), value.clone());
                                    Checkbox::new(SharedString::from(format!(
                                        "field-{name}-{n}"
                                    )))
                                    .label(title.clone())
                                    .checked(picked.contains(&value))
                                    .on_click(cx.listener(
                                        move |this, checked: &bool, _, cx| {
                                            let picks = this
                                                .picks
                                                .entry(key.clone())
                                                .or_default();
                                            if *checked {
                                                picks.insert(value.clone());
                                            } else {
                                                picks.remove(&value);
                                            }
                                            cx.notify();
                                        },
                                    ))
                                },
                            ))
                            .into_any_element()
                    }
                };
                let error = self.errors.get(&name).map(|error| match error {
                    FieldError::Required => {
                        rust_i18n::t!("Request.required").into_owned()
                    }
                    FieldError::TooShort(min) => {
                        rust_i18n::t!("Request.too_short", min = min)
                            .into_owned()
                    }
                    FieldError::TooLong(max) => {
                        rust_i18n::t!("Request.too_long", max = max)
                            .into_owned()
                    }
                    FieldError::Number => {
                        rust_i18n::t!("Request.bad_number").into_owned()
                    }
                    FieldError::Count => {
                        rust_i18n::t!("Request.bad_count").into_owned()
                    }
                });
                v_flex()
                    .gap_1()
                    .child(Label::new(label).text_sm())
                    .when_some(
                        field.description.clone(),
                        |this, description| {
                            this.child(
                                Label::new(description)
                                    .text_xs()
                                    .text_color(muted),
                            )
                        },
                    )
                    .child(input)
                    .when_some(error, |this, error| {
                        this.child(
                            Label::new(error).text_xs().text_color(danger),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        v_flex().gap_3().children(fields)
    }
}
