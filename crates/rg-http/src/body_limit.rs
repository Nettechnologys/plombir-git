/// Return whether an Axum body error was caused by a declared request-body
/// ceiling. `RequestBodyLimitLayer` wraps `LengthLimitError`, so callers must
/// walk the source chain instead of matching only the outer error.
pub(crate) fn is_length_limit_error(
    error: &(dyn std::error::Error + Send + Sync + 'static),
) -> bool {
    if error.is::<http_body_util::LengthLimitError>() {
        return true;
    }

    let mut current = error.source();
    while let Some(error) = current {
        if error.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        current = error.source();
    }
    false
}
