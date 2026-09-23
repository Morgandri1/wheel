// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Review of #136: a client's first page load fires several requests at once. Under
//! `provision=auto`, do concurrent first requests for the SAME new subject all succeed and
//! resolve to ONE Wheel principal?
#![cfg(feature = "sqlite")]

use wheel_api::auth::external::{self, Verified};
use wheel_api::config::{ExternalAuth, ExternalVerifier, Provision};
use wheel_api::db::Db;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_first_requests_for_one_new_subject_resolve_to_one_principal() {
    let path = std::env::temp_dir().join(format!("wheel-race-{}.db", uuid::Uuid::new_v4()));
    let db = std::sync::Arc::new(
        Db::connect(&format!("sqlite://{}", path.display()))
            .await
            .unwrap(),
    );
    let cfg = std::sync::Arc::new(ExternalAuth {
        provider: "test".into(),
        issuer: "https://idp.example".into(),
        audiences: vec!["wheel-test".into()],
        sole_audience: false,
        allow_issuer_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision: Provision::Auto,
        verifier: ExternalVerifier::ProxyHeader {
            subject_header: "x-forwarded-user".into(),
            email_header: None,
        },
    });

    let mut tasks = vec![];
    for _ in 0..8 {
        let (db, cfg) = (db.clone(), cfg.clone());
        tasks.push(tokio::spawn(async move {
            let v = Verified {
                subject: "alice".into(),
                email: None,
            };
            external::principal_for(&db, &cfg, &v)
                .await
                .map_err(|e| format!("{e:?}"))
        }));
    }
    let mut ok = vec![];
    let mut err = vec![];
    for t in tasks {
        match t.await.unwrap() {
            Ok(p) => ok.push(p),
            Err(e) => err.push(e),
        }
    }
    println!("ok={} err={} errors={err:?}", ok.len(), err.len());
    assert!(
        err.is_empty(),
        "{} of 8 concurrent first requests failed: {err:?}",
        err.len()
    );
    ok.dedup();
    assert_eq!(
        ok.len(),
        1,
        "concurrent first requests produced several principals: {ok:?}"
    );
    // The losers' accounts must not linger: nothing references them, and they would accumulate.
    let users = wheel_api::auth::local::count_users(&db).await.unwrap();
    assert_eq!(
        users, 1,
        "{users} user rows after one subject's first login; losers left orphans"
    );
}
