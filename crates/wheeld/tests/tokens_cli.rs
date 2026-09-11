// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `wheeld token` against a real store, and the first-boot operator token.
//!
//! In process, with the output captured, so every refusal can be held to saying what fixes it.

use std::path::PathBuf;
use uuid::Uuid;
use wheeld::tokens::{self, TokenCommand, OWNER_EMAIL};

fn data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wheeld-tok-{}", Uuid::new_v4().simple()));
    wheeld::supervise::prepare_data_dir(&dir).unwrap()
}

async fn store(dir: &std::path::Path) -> wheel_api::db::Db {
    wheel_api::db::Db::connect(&wheeld::supervise::store_url(dir))
        .await
        .unwrap()
}

async fn token(dir: &std::path::Path, cmd: TokenCommand) -> anyhow::Result<(String, String)> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    tokens::run(cmd, dir, &mut out, &mut err).await?;
    Ok((
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    ))
}

fn create(name: &str, email: Option<&str>) -> TokenCommand {
    TokenCommand::Create {
        name: name.into(),
        email: email.map(str::to_string),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn first_boot_writes_one_private_operator_token_and_only_once() {
    use std::os::unix::fs::PermissionsExt;
    let dir = data_dir();
    let db = store(&dir).await;

    let path = tokens::bootstrap_operator(&db, &dir)
        .await
        .unwrap()
        .expect("an empty store gets an operator token");
    assert_eq!(path, dir.join("operator-token"));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "operator-token is {mode:o}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.trim().starts_with("wht_"), "{written}");

    assert!(tokens::bootstrap_operator(&db, &dir)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        written,
        "a restart replaced the token"
    );

    let user = wheel_api::auth::api_token::verify(&db, written.trim())
        .await
        .unwrap();
    let owner = wheel_api::auth::local::find_token_only_user(&db, OWNER_EMAIL)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.user_id, owner.id.to_string());
}

#[tokio::test]
async fn create_prints_only_the_token_list_shows_it_and_revoke_ends_it() {
    let dir = data_dir();
    let db = store(&dir).await;
    tokens::bootstrap_operator(&db, &dir).await.unwrap();

    let (out, err) = token(&dir, create("laptop", None)).await.unwrap();
    let minted = out.trim().to_string();
    assert!(
        minted.starts_with("wht_") && !out.trim().contains('\n'),
        "stdout must be the token alone: {out:?}"
    );
    assert!(
        err.contains("laptop") && err.contains(OWNER_EMAIL) && !err.contains(&minted),
        "{err}"
    );
    let user = wheel_api::auth::api_token::verify(&db, &minted)
        .await
        .unwrap();

    let (listing, _) = token(&dir, TokenCommand::List).await.unwrap();
    assert!(listing.starts_with("ID"), "{listing}");
    assert!(!listing.contains("wht_"), "list printed a token: {listing}");
    let row = listing
        .lines()
        .find(|l| l.contains("laptop"))
        .expect("the new token is listed");
    assert!(row.contains(OWNER_EMAIL), "{row}");
    let id = row.split_whitespace().next().unwrap().to_string();

    let (_, said) = token(&dir, TokenCommand::Revoke { id: id.clone() })
        .await
        .unwrap();
    assert!(said.contains(&id), "{said}");
    assert!(
        wheel_api::auth::api_token::verify(&db, &minted)
            .await
            .is_err(),
        "a revoked token still works"
    );
    let (listing, _) = token(&dir, TokenCommand::List).await.unwrap();
    let row = listing.lines().find(|l| l.contains("laptop")).unwrap();
    assert!(
        !row.trim_end().ends_with('-'),
        "revocation is not shown: {row}"
    );
    let _ = user;
}

#[tokio::test]
async fn create_can_mint_for_an_existing_account_by_email() {
    let dir = data_dir();
    let db = store(&dir).await;
    let person = wheel_api::auth::local::create_user(&db, "person@example.com", "Correct-Horse-9!")
        .await
        .unwrap();

    let (out, _) = token(&dir, create("ui", Some("Person@Example.com")))
        .await
        .unwrap();
    let v = wheel_api::auth::api_token::verify(&db, out.trim())
        .await
        .unwrap();
    assert_eq!(v.user_id, person.id.to_string());
}

#[tokio::test]
async fn each_refusal_says_what_would_fix_it() {
    let missing = std::env::temp_dir().join(format!("wheeld-none-{}", Uuid::new_v4().simple()));
    let e = token(&missing, TokenCommand::List).await.unwrap_err();
    assert!(format!("{e:#}").contains("--data-dir"), "{e:#}");

    let dir = data_dir();
    let db = store(&dir).await;
    wheel_api::auth::local::create_user(&db, "first@example.com", "Correct-Horse-9!")
        .await
        .unwrap();
    assert!(tokens::bootstrap_operator(&db, &dir)
        .await
        .unwrap()
        .is_none());

    let e = token(&dir, create("x", None)).await.unwrap_err();
    assert!(format!("{e:#}").contains("--email"), "{e:#}");
    let e = token(&dir, create("x", Some("nobody@example.com")))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("nobody@example.com"), "{e:#}");
    let e = token(
        &dir,
        TokenCommand::Revoke {
            id: "not-a-uuid".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("not a token id"), "{e:#}");
    let e = token(
        &dir,
        TokenCommand::Revoke {
            id: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("no token"), "{e:#}");

    let (out, err) = token(&dir, TokenCommand::List).await.unwrap();
    assert!(
        out.is_empty() && err.contains("no tokens"),
        "{out:?} {err:?}"
    );
}
