import { API_BASE } from './_base.svelte';

function withWebSocketApiBase(path: string): string {
  const apiUrl = new URL(API_BASE, window.location.origin);
  const protocol = apiUrl.protocol === 'https:' ? 'wss:' : 'ws:';
  const basePath = apiUrl.pathname.replace(/\/+$/g, '');
  return `${protocol}//${apiUrl.host}${basePath}${path.startsWith('/') ? path : `/${path}`}`;
}

export function connectNotificationWebSocket(
  onMessage: (event: { event_type: string; data: any }) => void,
  onError?: (err: Event) => void,
): WebSocket | null {
  // WebSocket auth uses the HttpOnly cookie sent by the browser for same-origin
  // upgrades. The backend validates the cookie before accepting the connection.
  const ws = new WebSocket(withWebSocketApiBase('/ws/notifications'));

  ws.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      onMessage(data);
    } catch {
      // ignore non-JSON messages
    }
  };

  ws.onerror = (err) => {
    onError?.(err);
  };

  ws.onclose = () => {
    setTimeout(() => {
      connectNotificationWebSocket(onMessage, onError);
    }, 5000);
  };

  return ws;
}

export function connectJobLogWebSocket(
  jobId: number,
  onLog: (chunk: string) => void,
  onStatus?: (status: 'connected' | 'closed') => void,
  onError?: (err: Event) => void,
): WebSocket | null {
  const ws = new WebSocket(withWebSocketApiBase(`/ws/job/${jobId}`));

  ws.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      if (data.type === 'connected') {
        onStatus?.('connected');
        return;
      }
      if (data.event_type === 'job_log' && data.data?.job_id === jobId) {
        onLog(String(data.data.log ?? ''));
      }
    } catch {
      // ignore non-JSON messages
    }
  };

  ws.onerror = (err) => {
    onError?.(err);
  };

  ws.onclose = () => {
    onStatus?.('closed');
  };

  return ws;
}
