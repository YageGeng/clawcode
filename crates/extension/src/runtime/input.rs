use protocol::{InputEvent, InputResult};

use crate::{ExtensionContext, ExtensionRuntime, InputPoint};

impl ExtensionRuntime {
    /// Chains input text transformations and stops at the first handled result.
    pub async fn emit_input(
        &self,
        mut event: InputEvent,
        context: &ExtensionContext,
    ) -> InputResult {
        let mut transformed = false;
        for registered in self.handlers::<InputPoint>() {
            let handler_context = context.for_extension(registered.source());
            match registered.handler.handle(&event, &handler_context).await {
                Ok(InputResult::Continue) => {}
                Ok(InputResult::Transform { text }) => {
                    event.text = text;
                    transformed = true;
                }
                Ok(InputResult::Handled) => return InputResult::Handled,
                Err(error) => {
                    self.report::<InputPoint>(registered, &error).await;
                }
            }
        }

        if transformed {
            InputResult::Transform { text: event.text }
        } else {
            InputResult::Continue
        }
    }
}
