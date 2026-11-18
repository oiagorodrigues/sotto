use axum::{
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

use crate::oauth::AuthParams;

const CSS: &str = include_str!("../static/auth.css");

pub async fn stylesheet() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            ),
        ],
        CSS,
    )
        .into_response()
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn shell(title: &str, head_extra: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"dark\">\n\
         <title>{}</title>\n{head_extra}<link rel=\"stylesheet\" href=\"/static/auth.css\">\n</head>\n\
         <body>\n<main>\n{body}</main>\n</body>\n</html>\n",
        escape(title)
    )
}

fn hidden(p: &AuthParams, csrf: &str) -> String {
    let field = |name: &str, value: &str| {
        format!(
            "<input type=\"hidden\" name=\"{name}\" value=\"{}\">\n",
            escape(value)
        )
    };
    [
        field("csrf", csrf),
        field("response_type", &p.response_type),
        field("client_id", &p.client_id),
        field("redirect_uri", &p.redirect_uri),
        field("state", &p.state),
        field("code_challenge", &p.code_challenge),
        field("code_challenge_method", &p.code_challenge_method),
    ]
    .concat()
}

pub fn login_page(p: &AuthParams, csrf: &str, error: Option<&str>) -> String {
    let alert = error.map_or_else(String::new, |e| {
        format!("<p class=\"alert\" role=\"alert\">{}</p>\n", escape(e))
    });
    let h = hidden(p, csrf);
    let body = format!(
        "<h1>Sign in to Sotto</h1>\n{alert}\
         <form method=\"post\" action=\"/authorize/signin\">\n<h2>Sign in</h2>\n{h}\
         <label for=\"si-email\">Email</label>\n\
         <input id=\"si-email\" type=\"email\" name=\"email\" autocomplete=\"username\" required maxlength=\"254\">\n\
         <label for=\"si-password\">Password</label>\n\
         <input id=\"si-password\" type=\"password\" name=\"password\" autocomplete=\"current-password\" required maxlength=\"128\">\n\
         <button class=\"primary\" type=\"submit\">Sign in</button>\n</form>\n\
         <form method=\"post\" action=\"/authorize/magic\">\n<h2>Email me a link</h2>\n{h}\
         <label for=\"ml-email\">Email</label>\n\
         <input id=\"ml-email\" type=\"email\" name=\"email\" autocomplete=\"email\" required maxlength=\"254\">\n\
         <button type=\"submit\">Email me a link</button>\n</form>\n\
         <form method=\"post\" action=\"/authorize/signup\">\n<h2>Create an account</h2>\n{h}\
         <label for=\"su-email\">Email</label>\n\
         <input id=\"su-email\" type=\"email\" name=\"email\" autocomplete=\"email\" required maxlength=\"254\">\n\
         <label for=\"su-password\">Password (at least 10 characters)</label>\n\
         <input id=\"su-password\" type=\"password\" name=\"password\" autocomplete=\"new-password\" required minlength=\"10\" maxlength=\"128\">\n\
         <button type=\"submit\">Create account</button>\n</form>\n"
    );
    shell("Sign in to Sotto", "", &body)
}

pub fn message_page(title: &str, message: &str) -> String {
    let body = format!("<h1>{}</h1>\n<p>{}</p>\n", escape(title), escape(message));
    shell(title, "", &body)
}

pub fn check_email_page() -> String {
    message_page(
        "Check your email",
        "If the address can be used, we sent a link to it. The link works once and expires in 15 minutes.",
    )
}

/// `url` is built by the server from a random code and a validated `state`.
pub fn handoff_page(url: &str) -> String {
    let u = escape(url);
    let body = format!(
        "<h1>You are signed in</h1>\n\
         <p>Sotto should open now. If it does not, use the button below.</p>\n\
         <p><a class=\"button primary\" href=\"{u}\">Return to Sotto</a></p>\n\
         <p class=\"muted\">You can close this tab afterwards.</p>\n"
    );
    shell(
        "Return to Sotto",
        &format!("<meta http-equiv=\"refresh\" content=\"0;url={u}\">\n"),
        &body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_covers_html_specials() {
        assert_eq!(
            escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
