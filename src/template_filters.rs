//! Common templating filters

use matrix_sdk::ruma::MatrixToUri;

/// Inserts invisible space between first and remaining characters of provided [`std::fmt::Display`]-able object
/// # Errors
/// Shouldn't. Signature required by caller.
pub fn dehighlight_name<T: std::fmt::Display>(
    s: T,
    _: &dyn askama::Values,
) -> askama::Result<String> {
    let stringified = s.to_string();
    let mut chars = stringified.chars();
    Ok(chars.next().map_or_else(String::new, |first| {
        first.to_string() + "\u{200B}" + chars.as_str()
    }))
}

/// Formats room name + url as an `<a>` tag, if possible.
/// # Errors
/// Shouldn't. Signature required by caller.
pub fn maybe_room_url(
    (room_name, r): &(&String, &Option<MatrixToUri>),
    _: &dyn askama::Values,
) -> askama::Result<String> {
    match r {
        Some(url) => Ok(format!(r#"<a href="{url}">{room_name}</a>:   "#)),
        None => Ok(format!("{room_name}:   ")),
    }
}
