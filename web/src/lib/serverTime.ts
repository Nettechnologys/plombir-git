// Timestamps the server writes are UTC. Most API fields carry the zone
// (`2026-10-17T11:56:56Z`), but some are serialized from a naive UTC value and
// arrive without one (`2026-10-17 11:56:56.597504074`, pipelines and their jobs
// among them). `new Date()` reads a zone-less date-time as LOCAL time, so in
// UTC+2 such a value was two hours off — and a job that ran for one second
// showed a duration of `2h 1m`.

const NAIVE_DATE_TIME = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}(?::\d{2})?)(?:\.(\d+))?$/;

/** Parse a server timestamp, reading one without a zone as UTC. */
export function parseServerTime(value: string): Date {
	// Callers have always handed `new Date` whatever the API sent, an absent
	// optional field included; keep answering that exactly as `new Date` did.
	if (typeof value !== 'string') return new Date(value);
	const naive = NAIVE_DATE_TIME.exec(value.trim());
	if (!naive) return new Date(value);
	const [, date, time, fraction] = naive;
	// ECMAScript's date-time format takes at most milliseconds.
	const millis = fraction ? `.${fraction.slice(0, 3).padEnd(3, '0')}` : '';
	return new Date(`${date}T${time}${millis}Z`);
}
