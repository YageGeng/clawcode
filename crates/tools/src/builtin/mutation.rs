use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use tokio::sync::Mutex;

static FILE_MUTATION_QUEUES: LazyLock<
    Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Serializes mutations that resolve to the same real file while allowing other files in parallel.
pub(super) async fn with_file_mutation<T, F, Fut>(
    path: &Path,
    operation: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let key = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_error| path.to_path_buf());
    let queue = {
        let mut queues = FILE_MUTATION_QUEUES.lock().await;
        if let Some(queue) = queues.get(&key).and_then(Weak::upgrade) {
            queue
        } else {
            let queue = Arc::new(Mutex::new(()));
            queues.insert(key, Arc::downgrade(&queue));
            queue
        }
    };
    let _guard = queue.lock().await;
    operation().await
}
