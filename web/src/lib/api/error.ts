/** A non-successful HTTP response, kept distinct from transport failures. */
export class ApiError extends Error {
	readonly status: number;
	readonly code: string | null;
	readonly requestId: string | null;
	/**
	 * A machine-readable qualifier the client can act on rather than display —
	 * `sudo_required` is the one so far: the session is fine, it has to re-prove
	 * the password first. `null` for every error that has no such next step.
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
