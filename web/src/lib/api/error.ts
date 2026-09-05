/** A non-successful HTTP response, kept distinct from transport failures. */
export class ApiError extends Error {
	readonly status: number;
	readonly code: string | null;
	readonly requestId: string | null;

	constructor(message: string, status: number, code: string | null = null, requestId: string | null = null) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
		this.requestId = requestId;
	}
}
