use crate::completion::request::CompletionModel;

/// A provider client with completion capabilities.
/// Clone is required for conversions between client types.
pub trait CompletionClient {
    /// The type of CompletionModel used by the client.
    type CompletionModel: CompletionModel<Client = Self>;

    /// Create a completion model with the given model.
    fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel {
        Self::CompletionModel::make(self, model)
    }
}
