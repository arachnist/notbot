//! Common templating filters

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
