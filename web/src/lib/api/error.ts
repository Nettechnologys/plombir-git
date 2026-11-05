/** A non-successful HTTP response, kept distinct from transport failures. */
export class ApiError extends Error {
	readonly status: number;
	readonly code: string | null;
	readonly requestId: string | null;
	/**
	 * A machine-readable qualifier the client acts on rather than shows:
	 * `sudo_required` asks for fresh proof; `session_revoked` rejects the session;
	 * `identity_conflict` rejects credentials naming different accounts.
	 */
	readonly reason: string | null;

	constructor(
		message: string,
		status: number,
		code: string | null = null,
		requestId: string | null = null,
		reason: string | null = null,
	) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
		this.requestId = requestId;
		this.reason = reason;
	}
}
