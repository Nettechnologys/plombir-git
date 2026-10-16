//! `html_url`: the page a person opens to see a resource the API returned.
//!
//! A client that wants to show "here is your pull request" had to build the
//! address itself, out of the web app's route layout and an `external_url` it
//! does not know (card_dca6f5ecbc87). Routes of a single-page app are not a
//! contract — rename a page and every integration's links break in silence —
//! so the server, which knows both, says it.
//!
//! The base is `[server].external_url` and nothing else. Without it the field
//! is absent rather than a relative path or a guess from the request's `Host`:
//! the value ends up in bot notifications and agent reports read far from the
//! request that produced it, where an honest absence beats a link to nowhere.
//!
//! Every link is drawn from [`PAGE_ROUTES`], and the test at the bottom holds
//! each template to the web app: the page it names exists under
//! `web/src/routes`, and the fragment or query it carries is one that page
//! renders or reads.

use serde::Serialize;

/// Every kind of link [`Links`] builds, and its path below the base URL with
/// `{owner}`, `{repo}`, `{number}` and `{id}` placeholders.
pub(crate) const PAGE_ROUTES: &[(&str, &str)] = &[
    ("repo", "/{owner}/{repo}"),
    ("pull", "/{owner}/{repo}/pulls/{number}"),
    ("issue", "/{owner}/{repo}/issues/{number}"),
    (
        "issue_comment",
        "/{owner}/{repo}/issues/{number}#comment-{id}",
    ),
    (
        "review_comment",
        "/{owner}/{repo}/pulls/{number}#review-comment-{id}",
    ),
    ("pipeline", "/{owner}/{repo}/pipelines?pipeline={id}"),
    ("release", "/{owner}/{repo}/releases?release={id}"),
];

/// The links of one repository's resources, for one response.
///
/// `owner` and `repo` are the spelling the request resolved through, which
/// the web app resolves the same way the API did.
pub(crate) struct Links<'a> {
    base: Option<&'a str>,
    owner: &'a str,
    repo: &'a str,
}

impl<'a> Links<'a> {
    pub(crate) fn new(state: &'a crate::AppState, owner: &'a str, repo: &'a str) -> Self {
        Self::with_base(state.external_url.as_deref(), owner, repo)
    }

    pub(crate) fn with_base(base: Option<&'a str>, owner: &'a str, repo: &'a str) -> Self {
        let base = base
            .map(|base| base.trim().trim_end_matches('/'))
            .filter(|base| !base.is_empty());
        Self { base, owner, repo }
    }

    fn render(&self, kind: &str, number: Option<i64>, id: Option<i64>) -> Option<String> {
        let base = self.base?;
        let (_, template) = PAGE_ROUTES.iter().find(|(name, _)| *name == kind)?;
        let mut path = template
            .replace("{owner}", &urlencoding::encode(self.owner))
            .replace("{repo}", &urlencoding::encode(self.repo));
        if let Some(number) = number {
            path = path.replace("{number}", &number.to_string());
        }
        if let Some(id) = id {
            path = path.replace("{id}", &id.to_string());
        }
        Some(format!("{base}{path}"))
    }

    pub(crate) fn repo(&self) -> Option<String> {
        self.render("repo", None, None)
    }

    pub(crate) fn pull(&self, number: i64) -> Option<String> {
        self.render("pull", Some(number), None)
    }

    pub(crate) fn issue(&self, number: i64) -> Option<String> {
        self.render("issue", Some(number), None)
    }

    pub(crate) fn issue_comment(&self, issue_number: i64, comment_id: i64) -> Option<String> {
        self.render("issue_comment", Some(issue_number), Some(comment_id))
    }

    pub(crate) fn review_comment(&self, pr_number: i64, comment_id: i64) -> Option<String> {
        self.render("review_comment", Some(pr_number), Some(comment_id))
    }

    pub(crate) fn pipeline(&self, id: i64) -> Option<String> {
        self.render("pipeline", None, Some(id))
    }

    pub(crate) fn release(&self, id: i64) -> Option<String> {
        self.render("release", None, Some(id))
    }
}

/// `value` serialized, with `html_url` added when there is one — for the
/// responses that hand a database row to the client as it is.
pub(crate) fn with_html_url<T: Serialize>(value: T, html_url: Option<String>) -> serde_json::Value {
    let mut body = serde_json::json!(value);
    if let (Some(url), Some(fields)) = (html_url, body.as_object_mut()) {
        fields.insert("html_url".into(), url.into());
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_hang_off_the_configured_address_only() {
        let links = Links::with_base(Some("https://git.example.com/"), "alice", "app");
        assert_eq!(
            links.repo().as_deref(),
            Some("https://git.example.com/alice/app")
        );
        assert_eq!(
            links.pull(7).as_deref(),
            Some("https://git.example.com/alice/app/pulls/7")
        );
        assert_eq!(
            links.issue(3).as_deref(),
            Some("https://git.example.com/alice/app/issues/3")
        );
        assert_eq!(
            links.issue_comment(3, 41).as_deref(),
            Some("https://git.example.com/alice/app/issues/3#comment-41")
        );
        assert_eq!(
            links.review_comment(7, 9).as_deref(),
            Some("https://git.example.com/alice/app/pulls/7#review-comment-9")
        );
        assert_eq!(
            links.pipeline(12).as_deref(),
            Some("https://git.example.com/alice/app/pipelines?pipeline=12")
        );
        assert_eq!(
            links.release(5).as_deref(),
            Some("https://git.example.com/alice/app/releases?release=5")
        );

        // An instance mounted under a path keeps it.
        let nested = Links::with_base(Some("https://example.com/git"), "alice", "app");
        assert_eq!(
            nested.pull(1).as_deref(),
            Some("https://example.com/git/alice/app/pulls/1")
        );

        for base in [None, Some(""), Some("  ")] {
            let links = Links::with_base(base, "alice", "app");
            assert_eq!(links.repo(), None, "{base:?}");
            assert_eq!(links.pull(1), None, "{base:?}");
            assert_eq!(links.release(1), None, "{base:?}");
        }
    }

    #[test]
    fn a_name_cannot_step_out_of_its_segment() {
        let links = Links::with_base(Some("https://git.example.com"), "a/b", "c?d#e");
        assert_eq!(
            links.repo().as_deref(),
            Some("https://git.example.com/a%2Fb/c%3Fd%23e")
        );
    }

    #[test]
    fn the_body_gains_html_url_only_when_there_is_one() {
        let row = serde_json::json!({ "id": 1 });
        assert_eq!(
            with_html_url(&row, Some("https://x/a/b".into())),
            serde_json::json!({ "id": 1, "html_url": "https://x/a/b" })
        );
        assert_eq!(with_html_url(&row, None), serde_json::json!({ "id": 1 }));
    }

    /// The web app's page behind each kind of link: its directory under
    /// `web/src/routes`, and text its source must contain for the link's
    /// fragment or query to mean anything there — the element id it renders or
    /// the search parameter it reads.
    const SPA_PAGES: &[(&str, &str, Option<&str>)] = &[
        ("repo", "[owner]/[repo]", None),
        ("pull", "[owner]/[repo]/pulls/[number]", None),
        ("issue", "[owner]/[repo]/issues/[number]", None),
        (
            "issue_comment",
            "[owner]/[repo]/issues/[number]",
            Some("id=\"comment-{comment.id}\""),
        ),
        (
            "review_comment",
            "[owner]/[repo]/pulls/[number]",
            Some("id=\"review-comment-{comment.id}\""),
        ),
        (
            "pipeline",
            "[owner]/[repo]/pipelines",
            Some("searchParams.get('pipeline')"),
        ),
        (
            "release",
            "[owner]/[repo]/releases",
            Some("searchParams.get('release')"),
        ),
    ];

    /// card_dca6f5ecbc87: a link is a promise that a page answers it. Every
    /// template names a page the web app has, every placeholder in it is one
    /// [`Links`] fills, and a fragment or a query parameter is one the page
    /// renders or reads — so renaming a page or dropping an anchor turns this
    /// red instead of breaking every integration's links in silence.
    #[test]
    fn every_html_url_names_a_page_the_web_app_serves() {
        let routes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/routes");
        let links = Links::with_base(Some("https://git.example.com"), "o", "r");
        let rendered = [
            ("repo", links.repo()),
            ("pull", links.pull(1)),
            ("issue", links.issue(1)),
            ("issue_comment", links.issue_comment(1, 2)),
            ("review_comment", links.review_comment(1, 2)),
            ("pipeline", links.pipeline(2)),
            ("release", links.release(2)),
        ];
        assert_eq!(rendered.len(), PAGE_ROUTES.len());
        assert_eq!(SPA_PAGES.len(), PAGE_ROUTES.len());
        for (kind, url) in rendered {
            let url = url.unwrap_or_else(|| panic!("{kind} renders no link"));
            assert!(!url.contains('{'), "{kind} left a placeholder: {url}");
        }
        for (kind, template) in PAGE_ROUTES {
            let (_, spa_page, marker) = SPA_PAGES
                .iter()
                .find(|(name, _, _)| name == kind)
                .unwrap_or_else(|| panic!("{kind} names no page of the web app"));
            let page = routes.join(spa_page).join("+page.svelte");
            let source = std::fs::read_to_string(&page).unwrap_or_else(|error| {
                panic!(
                    "{kind} links to {template}, which has no page at {}: {error}",
                    page.display()
                )
            });
            // The template's path is the page's directory, segment by segment.
            let path = template.split(['?', '#']).next().unwrap();
            let directory = path
                .trim_start_matches('/')
                .replace('{', "[")
                .replace('}', "]");
            assert_eq!(&directory, spa_page, "{kind}");
            assert_eq!(
                template.contains(['?', '#']),
                marker.is_some(),
                "{kind}: a fragment or query needs a marker, and only then"
            );
            if let Some(marker) = marker {
                assert!(
                    source.contains(marker),
                    "{kind} links to {template}, but {} never contains {marker:?}",
                    page.display()
                );
            }
        }
    }
}
