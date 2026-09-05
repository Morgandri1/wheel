//! UID allocation for the process backend.
//!
//! A uid is a filesystem identity, so these tests are about identity confusion: no two projects
//! may share one, and a project must keep the same one for as long as its row exists. Getting
//! either wrong means one tenant inheriting another's files, which is the failure the whole
//! process backend exists to prevent.

use std::collections::HashSet;
use uuid::Uuid;
use wheel_host::store::Store;

const RANGE_START: u32 = 20_000;
const STRIDE: u32 = 64;

fn store() -> Store {
    let path = std::env::temp_dir().join(format!("wheel-uid-{}.db", Uuid::new_v4()));
    Store::open(path.to_str().unwrap()).expect("open store")
}

async fn project(s: &Store) -> Uuid {
    let id = Uuid::new_v4();
    s.upsert(&id, "engine-secret", "vault-key").await.unwrap();
    id
}

#[tokio::test]
async fn allocation_is_sticky_for_the_life_of_the_project() {
    // Re-allocating on every start would move a project's files out from under it.
    let s = store();
    let id = project(&s).await;

    let first = s.allocate_uid(&id, RANGE_START, STRIDE).await.unwrap();
    for _ in 0..5 {
        assert_eq!(
            s.allocate_uid(&id, RANGE_START, STRIDE).await.unwrap(),
            first,
            "a project's uid must not change once allocated"
        );
    }
    assert_eq!(s.get(&id).await.unwrap().unwrap().uid_base, Some(first));
}

#[tokio::test]
async fn ranges_do_not_overlap() {
    // Each project owns `stride` consecutive uids — base for the engine, the rest for its nodes.
    // Overlapping ranges would put two tenants' nodes on the same uid.
    let s = store();
    let mut bases = Vec::new();
    for _ in 0..8 {
        let id = project(&s).await;
        bases.push(s.allocate_uid(&id, RANGE_START, STRIDE).await.unwrap());
    }

    let unique: HashSet<_> = bases.iter().collect();
    assert_eq!(unique.len(), bases.len(), "a uid base was handed out twice");

    bases.sort_unstable();
    for pair in bases.windows(2) {
        assert!(
            pair[1] - pair[0] >= STRIDE,
            "ranges {}..{} and {}.. overlap",
            pair[0],
            pair[0] + STRIDE - 1,
            pair[1]
        );
    }
    assert!(
        bases[0] >= RANGE_START,
        "allocation started below the configured range"
    );
}

#[tokio::test]
async fn a_freed_uid_is_not_recycled_onto_a_new_project() {
    // The hazard: a new tenant receiving a uid whose old files are still on disk would own them.
    // Allocation therefore climbs; it does not reclaim gaps.
    let s = store();
    let first = project(&s).await;
    let base_a = s.allocate_uid(&first, RANGE_START, STRIDE).await.unwrap();
    let second = project(&s).await;
    let base_b = s.allocate_uid(&second, RANGE_START, STRIDE).await.unwrap();

    s.delete(&first).await.unwrap();

    let third = project(&s).await;
    let base_c = s.allocate_uid(&third, RANGE_START, STRIDE).await.unwrap();
    assert_ne!(base_c, base_a, "a deleted project's uid was recycled");
    assert!(
        base_c > base_b,
        "allocation should climb past every uid ever issued"
    );
}

#[tokio::test]
async fn concurrent_allocation_never_shares_a_uid() {
    // Reading max(uid_base) and then writing it separately is a race that would let two projects
    // agree on the same base. This is the test that would catch losing the transaction.
    let s = std::sync::Arc::new(store());
    let mut ids = Vec::new();
    for _ in 0..12 {
        ids.push(project(&s).await);
    }

    let mut set = tokio::task::JoinSet::new();
    for id in ids {
        let s = s.clone();
        set.spawn(async move { s.allocate_uid(&id, RANGE_START, STRIDE).await });
    }

    let mut bases = Vec::new();
    while let Some(joined) = set.join_next().await {
        // A collision may surface as the UNIQUE constraint rejecting the write rather than as a
        // duplicate; either way it must not produce two projects with one uid.
        if let Ok(Ok(base)) = joined {
            bases.push(base);
        }
    }

    let unique: HashSet<_> = bases.iter().collect();
    assert_eq!(
        unique.len(),
        bases.len(),
        "concurrent allocation handed the same uid to two projects: {bases:?}"
    );
}

#[tokio::test]
async fn allocating_for_an_unknown_project_is_an_error() {
    // Silently inventing a row here would let a uid exist with no project accountable for it.
    let s = store();
    assert!(s
        .allocate_uid(&Uuid::new_v4(), RANGE_START, STRIDE)
        .await
        .is_err());
}

#[tokio::test]
async fn a_zero_stride_is_refused() {
    // Stride 0 would make every project's range the single uid `base`.
    let s = store();
    let id = project(&s).await;
    assert!(s.allocate_uid(&id, RANGE_START, 0).await.is_err());
}

#[tokio::test]
async fn deleting_the_only_project_does_not_reset_the_counter() {
    // The case the previous test missed, and the one that was actually broken.
    //
    // `a_freed_uid_is_not_recycled_onto_a_new_project` keeps a second project alive, so a
    // max(uid_base)-based allocator still sees a high value and looks correct. Delete the *only*
    // project and the maximum falls back to nothing, handing the next tenant the uid whose files
    // are still on disk. That version shipped, and the only test that caught it needed root — so
    // this one exists to catch it on any machine.
    let s = store();
    let first = project(&s).await;
    let base = s.allocate_uid(&first, RANGE_START, STRIDE).await.unwrap();

    s.delete(&first).await.unwrap();
    assert!(
        s.get(&first).await.unwrap().is_none(),
        "precondition: the store should now hold no projects at all"
    );

    let second = project(&s).await;
    let next = s.allocate_uid(&second, RANGE_START, STRIDE).await.unwrap();
    assert!(
        next > base,
        "uid {next} was reissued after deleting the only project (previous {base}): a new tenant \
         would inherit the old one's files"
    );
}

#[tokio::test]
async fn the_watermark_survives_reopening_the_database() {
    // The host restarts. If the counter lived only in memory, or were recomputed from the rows,
    // every restart after a deletion would begin handing out used uids again.
    let path = std::env::temp_dir().join(format!("wheel-uid-persist-{}.db", Uuid::new_v4()));
    let path = path.to_str().unwrap().to_string();

    let base = {
        let s = Store::open(&path).unwrap();
        let id = Uuid::new_v4();
        s.upsert(&id, "e", "v").await.unwrap();
        let base = s.allocate_uid(&id, RANGE_START, STRIDE).await.unwrap();
        s.delete(&id).await.unwrap();
        base
    };

    let s = Store::open(&path).unwrap();
    let id = Uuid::new_v4();
    s.upsert(&id, "e", "v").await.unwrap();
    let next = s.allocate_uid(&id, RANGE_START, STRIDE).await.unwrap();
    assert!(
        next > base,
        "the watermark reset across a restart: {next} <= {base}"
    );
}
