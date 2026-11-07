//! Account page access sections, the admin's accounts page and the audit log.

use super::*;

impl Env {
    async fn post_as(&self, s: &Login, uri: &str, pairs: &[(&str, &str)]) -> Res {
        let mut all = vec![("csrf", s.csrf.as_str())];
        all.extend_from_slice(pairs);
        self.send_body(
            "POST",
            uri,
            "127.0.0.1",
            &[FORM, ("cookie", &s.cookie)],
            form(&all),
        )
        .await
    }
}

/// The value of the first read-only secret field on a shown-once page.
fn secret_of(page: &str, n: usize) -> String {
    let marker = format!("id=\"secret-{n}\" type=\"text\" readonly value=\"");
    let i = page.find(&marker).expect("secret field") + marker.len();
    page[i..].split('"').next().unwrap().to_string()
}

#[tokio::test]
async fn app_passwords_on_the_account_page() {
    let e = env();
    let s = e.session("Jana", "familie").await;
    let page = e.get_h("/.cc/account", &[("cookie", &s.cookie)]).await;
    assert_eq!(page.header("cache-control"), "no-store");
    assert!(page.text().contains("No app passwords yet."));

    let made = e
        .post_as(
            &s,
            "/.cc/account/app-passwords",
            &[("op", "add"), ("label", "laptop")],
        )
        .await;
    assert_eq!(made.status, StatusCode::OK);
    assert_eq!(made.header("cache-control"), "no-store");
    let secret = secret_of(&made.text(), 1);
    assert_eq!(secret_of(&made.text(), 0), "Jana");
    assert!(
        crate::auth::apppass::verify(&e.state.db, "jana", &secret)
            .unwrap()
            .is_some()
    );

    let dup = e
        .post_as(
            &s,
            "/.cc/account/app-passwords",
            &[("op", "add"), ("label", "laptop")],
        )
        .await;
    assert_eq!(
        dup.header("location"),
        "/.cc/account?app=duplicate#app-passwords"
    );

    let page = e
        .get_h("/.cc/account", &[("cookie", &s.cookie)])
        .await
        .text();
    assert!(page.contains("<strong>laptop</strong>"), "{page}");
    assert!(!page.contains(&secret), "the secret is shown only once");
    let id = e.state.db.app_passwords_of("jana").unwrap()[0].1.id.clone();
    let gone = e
        .post_as(
            &s,
            "/.cc/account/app-passwords",
            &[("op", "revoke"), ("id", &id)],
        )
        .await;
    assert_eq!(
        gone.header("location"),
        "/.cc/account?app=revoked#app-passwords"
    );
    assert!(e.state.db.app_passwords_of("jana").unwrap().is_empty());

    // Without the CSRF token nothing happens.
    let r = e
        .send_body(
            "POST",
            "/.cc/account/app-passwords",
            "127.0.0.1",
            &[FORM, ("cookie", &s.cookie)],
            form(&[("op", "add"), ("label", "x")]),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(e.state.db.app_passwords_of("jana").unwrap().is_empty());
}

#[tokio::test]
async fn sign_out_on_other_devices() {
    let e = env();
    let here = e.session("Jana", "familie").await;
    let phone = cookie_of(&e.login("jana", PW).await);
    let tablet = cookie_of(&e.login("jana", PW).await);
    let page = e
        .get_h("/.cc/account", &[("cookie", &here.cookie)])
        .await
        .text();
    assert!(page.contains("Sign out on the 2 other devices"), "{page}");

    let r = e.post_as(&here, "/.cc/account/sessions", &[]).await;
    assert_eq!(r.header("location"), "/.cc/account?signed_out=2#sessions");
    for gone in [&phone, &tablet] {
        let r = e.get_h("/Medien/", &[("cookie", gone)]).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "signed out");
    }
    let page = e
        .get_h("/.cc/account?signed_out=2", &[("cookie", &here.cookie)])
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Signed out on 2 other devices."));
    assert!(page.text().contains("You are signed in only here."));
    assert!(
        e.state
            .db
            .recent_audit(5)
            .unwrap()
            .iter()
            .any(|ev| ev.action == "sessions_ended" && ev.detail == "2")
    );
}

#[tokio::test]
async fn admin_manages_accounts_and_agents() {
    let e = env();
    let s = e.session("Tibor", "admin").await;
    let other = e.session("Kim", "leser").await;
    assert_eq!(
        e.get_h("/.cc/admin/users", &[("cookie", &other.cookie)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let r = e
        .post_as(
            &other,
            "/.cc/admin/users",
            &[("op", "add"), ("name", "x"), ("role", "leser")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let page = e.get_h("/.cc/admin/users", &[("cookie", &s.cookie)]).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Kim"));

    // New account: one-time password shown once, forced change.
    let made = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "add"), ("name", "Lena"), ("role", "familie")],
        )
        .await;
    assert_eq!(made.status, StatusCode::OK);
    assert_eq!(made.header("cache-control"), "no-store");
    let pw = secret_of(&made.text(), 1);
    let lena = e.state.db.user("lena").unwrap().unwrap();
    assert!(lena.must_change_password && lena.role == "familie");
    assert!(crate::auth::password::verify(&pw, &lena.password_hash));
    let again = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "add"), ("name", "LENA"), ("role", "familie")],
        )
        .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(again.text().contains("exists already"));

    // Role, block, unblock; never on oneself.
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "role"), ("name", "kim"), ("role", "familie")],
        )
        .await;
    assert_eq!(r.header("location"), "/.cc/admin/users?saved");
    assert_eq!(e.state.db.user("kim").unwrap().unwrap().role, "familie");
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "disable"), ("name", "kim")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(e.state.db.user("kim").unwrap().unwrap().disabled);
    assert_eq!(
        e.get_h("/Medien/", &[("cookie", &other.cookie)])
            .await
            .status,
        StatusCode::SEE_OTHER,
        "blocking ends the sessions"
    );
    for op in ["disable", "role"] {
        for me in ["Tibor", " tibor "] {
            let r = e
                .post_as(
                    &s,
                    "/.cc/admin/users",
                    &[("op", op), ("name", me), ("role", "leser")],
                )
                .await;
            assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{op} {me:?}");
        }
    }
    let me = e.state.db.user("tibor").unwrap().unwrap();
    assert!(!me.disabled && me.role == "admin");

    // New one-time password.
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "passwd"), ("name", "lena")],
        )
        .await;
    let pw2 = secret_of(&r.text(), 1);
    let lena = e.state.db.user("lena").unwrap().unwrap();
    assert!(crate::auth::password::verify(&pw2, &lena.password_hash));

    // Agents: never admin; a token shown once that the MCP gate accepts.
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "agent_add"), ("name", "bot"), ("role", "admin")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "agent_add"), ("name", "bot"), ("role", "leser")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[
                ("op", "agent_token"),
                ("name", "bot"),
                ("label", "aipc"),
                ("expires", "90"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let token = secret_of(&r.text(), 0);
    let tokens = e.state.db.agent_tokens().unwrap();
    assert_eq!(tokens.len(), 1);
    let rec = &tokens[0].1;
    assert_eq!(tokens[0].0, crate::auth::agent::hash(&token));
    assert!(rec.expires.is_some_and(|t| t > now() + 89 * 86_400));
    assert_eq!(rec.created_by, "user:tibor");
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[
                ("op", "agent_token"),
                ("name", "bot"),
                ("label", "x"),
                ("expires", "7"),
            ],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "only the offered expiries"
    );
    let page = e
        .get_h("/.cc/admin/users", &[("cookie", &s.cookie)])
        .await
        .text();
    assert!(page.contains("<strong>aipc</strong>") && !page.contains(&token));
    let r = e
        .post_as(
            &s,
            "/.cc/admin/users",
            &[("op", "agent_revoke"), ("name", "bot"), ("id", &rec.id)],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(e.state.db.agent_tokens().unwrap().is_empty());

    // Everything is in the audit log, with the admin as actor.
    let log = e
        .get_h(
            "/.cc/admin/audit?actor=user:tibor",
            &[("cookie", &s.cookie)],
        )
        .await
        .text();
    for action in [
        "user_created",
        "user_role",
        "user_disabled",
        "user_password_reset",
        "agent_created",
        "agent_token_created",
        "agent_token_revoked",
    ] {
        assert!(
            log.contains(&format!("<td>{action}</td>")),
            "{action} in {log}"
        );
    }
    let filtered = e
        .get_h(
            "/.cc/admin/audit?action=agent_token",
            &[("cookie", &s.cookie)],
        )
        .await
        .text();
    assert!(filtered.contains("agent_token_created") && !filtered.contains("<td>user_role</td>"));
    assert_eq!(
        e.get_h("/.cc/admin/audit", &[("cookie", &other.cookie)])
            .await
            .status,
        StatusCode::NOT_FOUND,
        "Kim is signed out (blocked); admin pages stay hidden"
    );
}
