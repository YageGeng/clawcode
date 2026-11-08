use kernel::SessionEventSinks;
use protocol::{AgentEventPayload, RunRequest};

use super::fixture::{Route, TestKernel};

/// Every tree navigation reports the moved branch to Session sinks, so
/// protocol projections can drop views captured before the move.
#[tokio::test]
async fn navigation_reports_branch_navigated() {
    let test = TestKernel::new(|_| Route::answer("done"));
    let session = test.create("tree-navigated").await;
    for input in ["first", "second"] {
        test.kernel
            .run(
                RunRequest {
                    session_id: session.clone(),
                    input: input.into(),
                },
                test.sinks.sink(&session),
            )
            .await
            .expect(input);
    }
    let tree = test.kernel.session_tree(&session).expect("tree");
    let leaf = tree.leaf_id.clone();
    let target = tree.entries.first().expect("first entry").entry_id.clone();

    test.kernel
        .navigate_session(&session, Some(target))
        .await
        .expect("navigate");
    test.kernel
        .navigate_session(&session, None)
        .await
        .expect("navigate to root");

    let navigations = test
        .sinks
        .payloads(&session)
        .into_iter()
        .filter_map(|payload| match payload {
            AgentEventPayload::BranchNavigated {
                old_leaf_id,
                new_leaf_id,
            } => Some((old_leaf_id, new_leaf_id)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(navigations.len(), 2, "{navigations:?}");
    assert_eq!(navigations.first().map(|(old, _)| old.clone()), Some(leaf));
    assert_eq!(navigations.last().map(|(_, new)| new.clone()), Some(None));
}
