pub(crate) mod pack_stream;
pub mod receive_pack;
pub mod upload_pack;
pub mod v2;

/// Maximum wire bytes retained while parsing one upload-pack negotiation.
/// Pack data flows in the opposite direction; this bounds only client-supplied
/// wants, haves, capabilities and command arguments.
pub const MAX_NEGOTIATION_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// Independent entry-count backstop for SSH and direct library callers, where
/// there is no HTTP request-body layer to enforce the byte ceiling above.
pub const MAX_NEGOTIATION_ENTRIES: usize = 100_000;
