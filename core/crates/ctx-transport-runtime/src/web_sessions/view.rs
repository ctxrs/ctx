use super::WebSessionInfo;

pub fn render_web_session_view(session: &WebSessionInfo, signal_endpoint: &str) -> String {
    fn escape_html(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace('\'', "&#39;")
    }

    fn escape_js_single_quoted(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029")
    }

    const TEMPLATE: &str = r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Web Session</title>
    <style>
      :root { color-scheme: dark; }
      body { margin: 0; background: #0b0b0b; color: #ddd; font-family: system-ui, sans-serif; }
      header { padding: 8px 12px; background: #111; font-size: 14px; display: flex; gap: 12px; align-items: center; }
      #status { font-size: 12px; opacity: 0.7; }
      #wrap { width: 100vw; height: calc(100vh - 36px); display: flex; align-items: center; justify-content: center; background: #000; }
      video { width: 100%; height: 100%; object-fit: contain; background: #000; cursor: default; }
    </style>
  </head>
  <body>
    <header>
      <div>Web Session: %%URL%%</div>
      <div id="status">connecting…</div>
    </header>
    <div id="wrap">
      <video id="view" autoplay playsinline muted></video>
    </div>
    <script>
      const status = document.getElementById('status');
      const video = document.getElementById('view');
      const VIEW_W = %%WIDTH%%;
      const VIEW_H = %%HEIGHT%%;
      let focused = false;
      let lastPoint = { x: VIEW_W / 2, y: VIEW_H / 2 };

      const signalEndpoint = '%%SIGNAL_ENDPOINT%%';
      const wsUrl = signalEndpoint.startsWith('ws://') || signalEndpoint.startsWith('wss://')
        ? signalEndpoint
        : (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + signalEndpoint;
      const ws = new WebSocket(wsUrl);
      const pc = new RTCPeerConnection({ iceServers: [{urls: 'stun:stun.l.google.com:19302'}] });

      pc.ontrack = (ev) => {
        const stream = ev.streams && ev.streams[0] ? ev.streams[0] : new MediaStream([ev.track]);
        if (video.srcObject !== stream) {
          video.srcObject = stream;
          video.play().catch(() => {});
        }
      };

      pc.onicecandidate = (ev) => {
        if (ev.candidate) ws.send(JSON.stringify({ type: 'candidate', candidate: ev.candidate }));
      };

      ws.addEventListener('open', async () => {
        status.textContent = 'signaling';
        pc.addTransceiver('video', { direction: 'recvonly' });
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        ws.send(JSON.stringify({ type: 'offer', sdp: offer.sdp }));
      });

      ws.addEventListener('message', async (ev) => {
        const msg = JSON.parse(ev.data);
        if (msg.type === 'answer') {
          await pc.setRemoteDescription({ type: 'answer', sdp: msg.sdp });
          status.textContent = 'connected';
        } else if (msg.type === 'candidate') {
          if (msg.candidate) await pc.addIceCandidate(msg.candidate);
        } else if (msg.type === 'cursor') {
          updateCursor(msg.cursor);
        }
      });

      function mods(ev) {
        let m = 0;
        if (ev.altKey) m |= 1;
        if (ev.ctrlKey) m |= 2;
        if (ev.metaKey) m |= 4;
        if (ev.shiftKey) m |= 8;
        return m;
      }

      function mapCoords(ev) {
        const rect = video.getBoundingClientRect();
        const actualW = video.videoWidth || VIEW_W;
        const actualH = video.videoHeight || VIEW_H;
        const videoAspect = actualW / actualH;
        const rectAspect = rect.width / rect.height;
        let displayW = rect.width;
        let displayH = rect.height;
        let offsetX = 0;
        let offsetY = 0;
        if (rectAspect > videoAspect) {
          displayH = rect.height;
          displayW = rect.height * videoAspect;
          offsetX = (rect.width - displayW) / 2;
        } else {
          displayW = rect.width;
          displayH = rect.width / videoAspect;
          offsetY = (rect.height - displayH) / 2;
        }
        const x = (ev.clientX - rect.left - offsetX) * actualW / displayW;
        const y = (ev.clientY - rect.top - offsetY) * actualH / displayH;
        const mapped = { x: Math.max(0, Math.min(actualW, x)), y: Math.max(0, Math.min(actualH, y)) };
        lastPoint = mapped;
        return mapped;
      }

      function buttonName(button) {
        if (button === 1) return 'middle';
        if (button === 2) return 'right';
        return 'left';
      }

      function send(msg) {
        if (ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify(msg));
        }
      }

      let lastCursor = 'default';
      let cursorTimer = null;
      function startCursorProbe() {
        if (cursorTimer) return;
        cursorTimer = setInterval(() => {
          if (!focused) return;
          send({ type: 'cursor_probe', x: lastPoint.x, y: lastPoint.y });
        }, 120);
      }

      function updateCursor(cursor) {
        if (!cursor || cursor === lastCursor) return;
        lastCursor = cursor;
        video.style.cursor = cursor;
      }

      video.addEventListener('mousedown', (ev) => {
        ev.preventDefault();
        focused = true;
        const { x, y } = mapCoords(ev);
        send({ type: 'mouse', event: 'down', x, y, button: buttonName(ev.button), buttons: ev.buttons, clickCount: ev.detail, modifiers: mods(ev) });
        send({ type: 'cursor_probe', x, y });
        startCursorProbe();
      });
      video.addEventListener('mouseup', (ev) => {
        ev.preventDefault();
        const { x, y } = mapCoords(ev);
        send({ type: 'mouse', event: 'up', x, y, button: buttonName(ev.button), buttons: ev.buttons, clickCount: ev.detail, modifiers: mods(ev) });
      });
      video.addEventListener('mousemove', (ev) => {
        const { x, y } = mapCoords(ev);
        send({ type: 'mouse', event: 'move', x, y, buttons: ev.buttons, modifiers: mods(ev) });
        send({ type: 'cursor_probe', x, y });
      });
      video.addEventListener('wheel', (ev) => {
        ev.preventDefault();
        const { x, y } = mapCoords(ev);
        send({ type: 'mouse', event: 'wheel', x, y, deltaX: ev.deltaX, deltaY: ev.deltaY, modifiers: mods(ev) });
      }, { passive: false });
      video.addEventListener('contextmenu', (ev) => ev.preventDefault());

      window.addEventListener('keydown', (ev) => {
        if (!focused) return;
        ev.preventDefault();
        const modifiers = mods(ev);
        const text = (modifiers === 0 && ev.key && ev.key.length === 1) ? ev.key : '';
        const raw = modifiers !== 0 || !text;
        send({ type: 'key', event: 'down', key: ev.key, code: ev.code, keyCode: ev.keyCode, text, modifiers, raw });
      });
      window.addEventListener('keyup', (ev) => {
        if (!focused) return;
        ev.preventDefault();
        const modifiers = mods(ev);
        send({ type: 'key', event: 'up', key: ev.key, code: ev.code, keyCode: ev.keyCode, modifiers });
      });
    </script>
  </body>
</html>
"#;

    TEMPLATE
        .replace("%%URL%%", &escape_html(&session.url))
        .replace("%%WIDTH%%", &session.viewport.width.to_string())
        .replace("%%HEIGHT%%", &session.viewport.height.to_string())
        .replace(
            "%%SIGNAL_ENDPOINT%%",
            &escape_js_single_quoted(signal_endpoint),
        )
}
