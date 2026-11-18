//! The MongoDB driver's syntax check of a `uri`, run at connect and on save.

#[cfg(feature = "mongodb")]
use dbento_workspace::MONGO_BAD_PORT as BAD_PORT;

// The driver's own syntax check, run at connect and on save. Its messages quote the text, even with
// no `@` (`u:secret/` reads as a host and port), so only the port error passes, in fixed words.
#[cfg(feature = "mongodb")]
pub(crate) fn driver_syntax(uri: &str) -> Result<(), String> {
    use mongodb::error::ErrorKind;
    use mongodb::options::ConnectionString;

    let Err(error) = ConnectionString::parse(uri) else {
        return Ok(());
    };
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "ErrorKind has too many variants to list; the rest share one arm"
    )]
    match *error.kind {
        ErrorKind::InvalidArgument { message, .. }
            if !uri.contains('@') && message.contains("the port must be an integer") =>
        {
            Err(BAD_PORT.to_owned())
        }
        _ => Err(
            "`uri` is not a MongoDB connection string (the driver's reason is hidden \
                  because the text may hold a password)"
                .to_owned(),
        ),
    }
}

#[cfg(not(feature = "mongodb"))]
pub(crate) fn driver_syntax(_uri: &str) -> Result<(), String> {
    Ok(())
}

// Separate attributes, so clippy still sees the module as test code.
#[cfg(test)]
#[cfg(feature = "mongodb")]
mod tests;
