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

/// A request refused on the client's account: an object the advertisement did
/// not offer, an argument the protocol forbids, more negotiation than the
/// server accepts.
///
/// Not a failure of the server's. The client is told the reason in a git `ERR`
/// packet, the way stock `upload-pack` tells it, and the transports answer it
/// as a delivered protocol response logged at warn — not as the `500` with an
/// error-level log line that pages an operator for a client asking for what
/// it may not have (card_bd1b7010d482).
#[derive(Debug)]
pub struct ClientRefusal(String);

impl ClientRefusal {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }

    /// The sentence the client reads after `ERR `.
    pub fn reason(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClientRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientRefusal {}

/// The refusal anywhere in `error`'s chain, if the error is one.
pub fn client_refusal(error: &anyhow::Error) -> Option<&ClientRefusal> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ClientRefusal>())
}

/// Hand a refusal to the client as an `ERR` packet before it leaves the
/// handler, leaving every other outcome as it was.
///
/// Refusals are raised before any pack byte is written, so the packet never
/// lands inside a sideband stream.
pub(crate) async fn tell_client_of_refusal<W, T>(
    writer: &mut W,
    outcome: anyhow::Result<T>,
) -> anyhow::Result<T>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use anyhow::Context;
    use tokio::io::AsyncWriteExt;

    let error = match outcome {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let Some(refusal) = client_refusal(&error) else {
        return Err(error);
    };
    crate::pkt_line::write_pkt_line(
        writer,
        &crate::pkt_line::PktLine::text(&format!("ERR {}", refusal.reason())),
    )
    .await
    .context("failed to send the refusal to the client")?;
    writer
        .flush()
        .await
        .context("failed to send the refusal to the client")?;
    Err(error)
}
