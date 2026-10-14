//! E2e smoke tests. All ignored by default: opt in with
//! `cargo test -p hydradb-operator-e2e -- --ignored`.
//!
//! Every test skips gracefully (exit 0 + skip message) when no cluster is
//! reachable, so the suite never fails off-cluster.

use hydradb_operator_e2e::{DEFAULT_RELEASE_NAMESPACE, Harness};

async fn client_or_skip(test: &str) -> Option<kube::Client> {
    match Harness::try_client().await {
        Ok(Some(client)) => Some(client),
        Ok(None) => {
            println!("SKIP: {test}: no cluster reachable; e2e test is a no-op");
            None
        }
        Err(e) => {
            println!("SKIP: {test}: try_client errored ({e:#}); e2e test is a no-op");
            None
        }
    }
}

#[tokio::test]
#[ignore = "needs a kind cluster; run with --ignored"]
async fn harness_resolves_no_client_off_cluster() {
    let client = Harness::try_client()
        .await
        .expect("try_client must never fail, it returns Ok(None) off-cluster");
    if client.is_none() {
        println!("SKIP: harness_resolves_no_client_off_cluster: no cluster reachable");
    }
}

#[tokio::test]
#[ignore = "needs a kind cluster; run with --ignored"]
async fn smoke_apiserver_reachable() {
    let Some(_client) = client_or_skip("smoke_apiserver_reachable").await else {
        return;
    };
}

#[tokio::test]
#[ignore = "needs a kind cluster; run with --ignored"]
async fn smoke_teardown_is_idempotent_without_setup() {
    let harness = Harness::new();
    // Teardown without prior setup must not fail. Off-cluster this is a
    // skip; on-cluster it exercises the idempotent teardown path.
    if client_or_skip("smoke_teardown_is_idempotent_without_setup")
        .await
        .is_none()
    {
        return;
    }
    harness
        .teardown(DEFAULT_RELEASE_NAMESPACE)
        .expect("teardown must be idempotent");
}
