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
  shouldReconnect: () => boolean = () => true,
  onStatus?: (connected: boolean) => void,
): { disconnect: () => void } {
  let active = true;
  let socket: WebSocket | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  function openSocket() {
    if (!active) return;

    // WebSocket auth uses the HttpOnly cookie sent by the browser for same-origin
    // upgrades. The backend validates the cookie before accepting the connection.
    const ws = new WebSocket(withWebSocketApiBase('/ws/notifications'));
    socket = ws;
    let opened = false;

    ws.onopen = () => {
      if (!active || socket !== ws) return;
      opened = true;
      onStatus?.(true);
    };

    ws.onmessage = (event) => {
      if (!active || socket !== ws) return;
      try {
        const data = JSON.parse(event.data);
        onMessage(data);
      } catch {
        // ignore non-JSON messages
      }
    };

    ws.onerror = (err) => {
      if (!active || socket !== ws) return;
      onError?.(err);
    };

    ws.onclose = () => {
      if (socket === ws) socket = null;
      if (!active) return;
      onStatus?.(false);

      // A failed handshake (including 401) never reaches `open`. Retrying that
      // shape forever only repeats an answer that JavaScript cannot inspect.
      if (!opened) return;

      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        if (!active || !shouldReconnect()) return;
        openSocket();
      }, 5000);
    };
  }

  openSocket();

  return {
    disconnect() {
      if (!active) return;
      active = false;
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      const currentSocket = socket;
      socket = null;
      currentSocket?.close();
      onStatus?.(false);
    },
  };
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
