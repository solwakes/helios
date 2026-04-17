/// The Helios HTML dashboard — a single self-contained page that renders the
/// live OS state in a browser. Polls /stats every 2s and /tree every 5s.
/// Zero external dependencies. All CSS + JS is inline.
///
/// Served at `GET /dashboard` by `http::route_read`.

/// The full HTML document, as a raw string. Kept under ~16 KB.
pub const DASHBOARD_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>HELIOS · dashboard</title>
<style>
  :root {
    --bg:      #1a1b2e;
    --bg-2:    #232444;
    --bg-3:    #2d2f58;
    --fg:      #e8e8f0;
    --muted:   #8a8ca8;
    --amber:   #ffb020;
    --green:   #5dff7f;
    --red:     #ff5370;
    --blue:    #80b0ff;
    --line:    #3a3c68;
  }
  * { box-sizing: border-box; }
  html, body {
    margin: 0; padding: 0;
    background: var(--bg);
    color: var(--fg);
    font-family: 'SF Mono', Menlo, 'DejaVu Sans Mono', Consolas, monospace;
    font-size: 13px;
    line-height: 1.45;
    min-height: 100vh;
  }
  a { color: var(--amber); text-decoration: none; }
  a:hover { text-decoration: underline; }

  header {
    background: linear-gradient(180deg, var(--bg-2), var(--bg));
    border-bottom: 1px solid var(--line);
    padding: 12px 20px;
    display: flex;
    align-items: baseline;
    gap: 20px;
  }
  header h1 {
    margin: 0;
    font-size: 22px;
    letter-spacing: 4px;
    color: var(--amber);
    text-shadow: 0 0 8px rgba(255,176,32,0.3);
  }
  header .tag { color: var(--muted); font-size: 12px; }
  header .uptime { margin-left: auto; color: var(--green); }
  header .version { color: var(--muted); }

  main {
    display: grid;
    grid-template-columns: 1fr 1.2fr 1fr;
    gap: 0;
    min-height: calc(100vh - 52px - 120px);
  }
  @media (max-width: 900px) {
    main { grid-template-columns: 1fr; }
  }

  section {
    border-right: 1px solid var(--line);
    padding: 14px 18px;
    overflow: auto;
  }
  section:last-child { border-right: 0; }
  section h2 {
    margin: 0 0 10px 0;
    font-size: 11px;
    letter-spacing: 2px;
    color: var(--amber);
    text-transform: uppercase;
    border-bottom: 1px dashed var(--line);
    padding-bottom: 6px;
  }

  /* ── Tree ─────────────────────────────────────────── */
  ul.tree, ul.tree ul {
    list-style: none;
    margin: 0;
    padding-left: 14px;
  }
  ul.tree { padding-left: 0; }
  ul.tree li { margin: 1px 0; }
  .node-row {
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 3px;
    display: inline-block;
  }
  .node-row:hover { background: var(--bg-2); }
  .node-row.sel { background: var(--bg-3); color: var(--amber); }
  .tri { display: inline-block; width: 12px; color: var(--muted); }
  .nid { color: var(--muted); }
  .ntype {
    display: inline-block;
    font-size: 10px;
    padding: 0 4px;
    margin-right: 6px;
    border: 1px solid var(--line);
    border-radius: 2px;
    color: var(--muted);
  }
  .nname { color: var(--fg); }
  .ntype.User { color: var(--green); border-color: var(--green); }
  .ntype.Directory { color: var(--blue); border-color: var(--blue); }
  .ntype.Text { color: var(--fg); }
  .ntype.System { color: var(--amber); border-color: var(--amber); }

  /* ── Detail ───────────────────────────────────────── */
  .detail-head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 10px;
  }
  .detail-head .name {
    color: var(--amber);
    font-size: 16px;
    font-weight: bold;
  }
  .detail-head .id { color: var(--muted); }
  .detail-meta {
    color: var(--muted);
    font-size: 11px;
    margin-bottom: 10px;
  }
  .detail-meta span { margin-right: 14px; }
  pre.content {
    background: #10101e;
    border: 1px solid var(--line);
    border-radius: 3px;
    padding: 10px;
    color: var(--green);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 360px;
    overflow: auto;
    font-size: 12px;
  }
  .edges { margin-top: 12px; }
  .edges h3 {
    font-size: 11px;
    letter-spacing: 1px;
    color: var(--muted);
    margin: 0 0 6px 0;
    text-transform: uppercase;
  }
  .edges a { color: var(--blue); }
  .edge-row {
    padding: 2px 0;
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .edge-row .lbl { color: var(--muted); min-width: 48px; }

  button, .btn {
    background: var(--bg-2);
    color: var(--fg);
    border: 1px solid var(--line);
    padding: 5px 10px;
    font-family: inherit;
    font-size: 12px;
    border-radius: 3px;
    cursor: pointer;
  }
  button:hover, .btn:hover { border-color: var(--amber); color: var(--amber); }
  button.danger { border-color: var(--red); color: var(--red); }
  button.danger:hover { background: var(--red); color: var(--bg); }

  /* ── Stats ────────────────────────────────────────── */
  table.stats { width: 100%; border-collapse: collapse; }
  table.stats td { padding: 3px 4px; vertical-align: top; }
  table.stats td.k { color: var(--muted); width: 48%; }
  table.stats td.v { color: var(--green); text-align: right; }
  .stats-group {
    margin-bottom: 14px;
    border: 1px solid var(--line);
    border-radius: 3px;
    padding: 8px 10px;
    background: var(--bg-2);
  }
  .stats-group > .title {
    font-size: 10px;
    letter-spacing: 2px;
    color: var(--amber);
    text-transform: uppercase;
    margin-bottom: 4px;
  }
  .bar {
    height: 6px;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 2px;
    overflow: hidden;
    margin-top: 3px;
  }
  .bar > div {
    height: 100%;
    background: linear-gradient(90deg, var(--green), var(--amber));
  }

  /* ── Creator form ─────────────────────────────────── */
  footer {
    border-top: 1px solid var(--line);
    background: var(--bg-2);
    padding: 12px 20px;
  }
  footer h2 {
    margin: 0 0 8px 0;
    font-size: 11px;
    letter-spacing: 2px;
    color: var(--amber);
    text-transform: uppercase;
  }
  footer form {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  footer input, footer select, footer textarea {
    background: var(--bg);
    color: var(--fg);
    border: 1px solid var(--line);
    padding: 5px 8px;
    font-family: inherit;
    font-size: 12px;
    border-radius: 3px;
  }
  footer input:focus, footer select:focus, footer textarea:focus {
    outline: none;
    border-color: var(--amber);
  }
  footer textarea { flex: 1 1 280px; min-width: 200px; height: 32px; resize: vertical; }
  footer .row { display: flex; gap: 8px; align-items: center; width: 100%; flex-wrap: wrap; }
  #msg { color: var(--green); font-size: 11px; margin-left: 8px; }
  #msg.err { color: var(--red); }

  .muted { color: var(--muted); }
  .pulse { animation: pulse 1.5s infinite; }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }
</style>
</head>
<body>

<header>
  <h1>HELIOS</h1>
  <span class="tag">Everything is a memory.</span>
  <span class="uptime" id="uptime">—</span>
  <span class="version" id="version">v?</span>
</header>

<main>
  <section id="tree-pane">
    <h2>/ graph tree <span class="muted" id="tree-age"></span></h2>
    <div id="tree">loading…</div>
  </section>

  <section id="detail-pane">
    <h2>/ node</h2>
    <div id="detail"><span class="muted">click a node in the tree ←</span></div>
  </section>

  <section id="stats-pane">
    <h2>/ stats <span class="muted pulse" id="stats-age">●</span></h2>
    <div id="stats">loading…</div>
  </section>
</main>

<footer>
  <h2>+ create node (under /user)</h2>
  <form id="create-form">
    <div class="row">
      <input type="text" id="f-name" placeholder="name" required>
      <select id="f-type">
        <option value="note">note</option>
        <option value="dir">dir</option>
      </select>
      <textarea id="f-content" placeholder="content (optional)"></textarea>
      <button type="submit">create</button>
      <span id="msg"></span>
    </div>
  </form>
</footer>

<script>
(() => {
  let selectedId = null;
  let lastTree = null;
  let expanded = new Set([1, 12]); // root + /user expanded by default

  // ── Utilities ─────────────────────────────────────────
  function esc(s) {
    return String(s ?? '')
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function fmtBytes(n) {
    if (n < 1024) return n + ' B';
    if (n < 1024*1024) return (n/1024).toFixed(1) + ' KB';
    return (n/1024/1024).toFixed(2) + ' MB';
  }

  function fmtDuration(s) {
    s = Math.floor(s);
    const h = Math.floor(s/3600);
    const m = Math.floor((s%3600)/60);
    const sec = s%60;
    if (h > 0) return h + 'h ' + m + 'm ' + sec + 's';
    if (m > 0) return m + 'm ' + sec + 's';
    return sec + 's';
  }

  async function jfetch(url, opts) {
    const r = await fetch(url, { cache: 'no-cache', ...opts });
    if (!r.ok) throw new Error(url + ' → ' + r.status);
    const ct = r.headers.get('content-type') || '';
    if (ct.includes('json')) return r.json();
    return r.text();
  }

  // ── Stats panel ───────────────────────────────────────
  async function refreshStats() {
    try {
      const s = await jfetch('/stats');

      document.getElementById('uptime').textContent = 'up ' + fmtDuration(s.uptime_s || 0);

      const used = s.heap?.used || 0;
      const total = s.heap?.total || 1;
      const pct = Math.round(100 * used / total);

      const html = `
        <div class="stats-group">
          <div class="title">heap</div>
          <table class="stats">
            <tr><td class="k">used</td><td class="v">${fmtBytes(used)}</td></tr>
            <tr><td class="k">free</td><td class="v">${fmtBytes(s.heap?.free || 0)}</td></tr>
            <tr><td class="k">total</td><td class="v">${fmtBytes(total)}</td></tr>
          </table>
          <div class="bar"><div style="width:${pct}%"></div></div>
        </div>

        <div class="stats-group">
          <div class="title">graph</div>
          <table class="stats">
            <tr><td class="k">nodes</td><td class="v">${s.graph?.nodes || 0}</td></tr>
            <tr><td class="k">edges</td><td class="v">${s.graph?.edges || 0}</td></tr>
            <tr><td class="k">user-created</td><td class="v">${s.graph?.user_nodes || 0}</td></tr>
          </table>
        </div>

        <div class="stats-group">
          <div class="title">http</div>
          <table class="stats">
            <tr><td class="k">requests</td><td class="v">${s.http?.requests || 0}</td></tr>
            <tr><td class="k">writes</td><td class="v">${s.http?.writes || 0}</td></tr>
            <tr><td class="k">bytes out</td><td class="v">${fmtBytes(s.http?.bytes_out || 0)}</td></tr>
            <tr><td class="k">404s</td><td class="v">${s.http?.not_found || 0}</td></tr>
            <tr><td class="k">errors</td><td class="v">${s.http?.errors || 0}</td></tr>
          </table>
        </div>

        <div class="stats-group">
          <div class="title">tcp</div>
          <table class="stats">
            <tr><td class="k">accepts</td><td class="v">${s.tcp?.accepts || 0}</td></tr>
            <tr><td class="k">rx / tx segs</td><td class="v">${s.tcp?.rx_segments || 0} / ${s.tcp?.tx_segments || 0}</td></tr>
            <tr><td class="k">rx / tx bytes</td><td class="v">${fmtBytes(s.tcp?.rx_bytes || 0)} / ${fmtBytes(s.tcp?.tx_bytes || 0)}</td></tr>
            <tr><td class="k">closes</td><td class="v">${s.tcp?.closes || 0}</td></tr>
            <tr><td class="k">retransmits</td><td class="v">${s.tcp?.retransmits || 0}</td></tr>
          </table>
        </div>

        <div class="stats-group">
          <div class="title">net</div>
          <table class="stats">
            <tr><td class="k">rx / tx frames</td><td class="v">${s.net?.rx_frames || 0} / ${s.net?.tx_frames || 0}</td></tr>
            <tr><td class="k">arp rx/tx</td><td class="v">${s.net?.arp_rx || 0} / ${s.net?.arp_tx || 0}</td></tr>
            <tr><td class="k">icmp rx/tx</td><td class="v">${s.net?.icmp_rx || 0} / ${s.net?.icmp_tx || 0}</td></tr>
          </table>
        </div>

        <div class="stats-group">
          <div class="title">tasks (${(s.tasks||[]).length})</div>
          <table class="stats">
            ${(s.tasks||[]).map(t =>
              `<tr><td class="k">${esc(t.name)}</td><td class="v">${esc(t.state)} · ${t.preempts}</td></tr>`
            ).join('')}
          </table>
        </div>
      `;
      document.getElementById('stats').innerHTML = html;
      document.getElementById('stats-age').textContent = '●';
    } catch (e) {
      document.getElementById('stats-age').textContent = '✕';
    }
  }

  // ── Tree panel ────────────────────────────────────────
  function renderTree(node, depth = 0) {
    if (!node) return '';
    const hasChildren = node.children && node.children.length > 0;
    const isOpen = expanded.has(node.id);
    const tri = hasChildren ? (isOpen ? '▼' : '▶') : ' ';
    const type = node.type || '';
    const sel = (node.id === selectedId) ? ' sel' : '';

    let out = '<li>';
    out += `<span class="node-row${sel}" data-id="${node.id}" data-has="${hasChildren ? 1 : 0}">`;
    out += `<span class="tri">${tri}</span>`;
    out += `<span class="ntype ${esc(type)}">${esc(type.slice(0,3).toLowerCase())}</span>`;
    out += `<span class="nname">${esc(node.name)}</span> `;
    out += `<span class="nid">#${node.id}</span>`;
    out += `</span>`;

    if (hasChildren && isOpen) {
      out += '<ul>';
      for (const ch of node.children) {
        if (ch && ch.id) out += renderTree(ch, depth + 1);
      }
      out += '</ul>';
    }
    out += '</li>';
    return out;
  }

  async function refreshTree() {
    try {
      const t = await jfetch('/tree');
      lastTree = t;
      document.getElementById('tree').innerHTML = '<ul class="tree">' + renderTree(t) + '</ul>';
      wireTreeClicks();
    } catch (e) {
      document.getElementById('tree').innerHTML = '<span class="muted">tree error: ' + esc(e.message) + '</span>';
    }
  }

  function wireTreeClicks() {
    document.querySelectorAll('.node-row').forEach(el => {
      el.addEventListener('click', ev => {
        const id = Number(el.dataset.id);
        const has = el.dataset.has === '1';
        // Toggle expand on disclosure triangle area or dblclick, select otherwise.
        // Simplification: clicking same-node toggles; clicking new node selects.
        if (selectedId === id && has) {
          if (expanded.has(id)) expanded.delete(id); else expanded.add(id);
          rerenderTreeLocal();
        } else {
          selectedId = id;
          if (has && !expanded.has(id)) expanded.add(id);
          rerenderTreeLocal();
          showDetail(id);
        }
        ev.stopPropagation();
      });
    });
  }

  function rerenderTreeLocal() {
    if (!lastTree) return;
    document.getElementById('tree').innerHTML = '<ul class="tree">' + renderTree(lastTree) + '</ul>';
    wireTreeClicks();
  }

  // ── Detail panel ──────────────────────────────────────
  async function showDetail(id) {
    const el = document.getElementById('detail');
    el.innerHTML = '<span class="muted">loading #' + id + '…</span>';
    try {
      const n = await jfetch('/nodes/' + id);
      const userFlag = n.user ? ' <span class="ntype User">user</span>' : '';
      const delBtn = n.user
        ? `<button class="danger" id="del-btn" data-id="${n.id}">delete</button>`
        : '';
      const edges = (n.edges || []).map(e =>
        `<div class="edge-row">
          <span class="lbl">${esc(e.label)}</span>
          <a href="#" data-id="${e.target}" class="edge-link">#${e.target} ${esc(e.target_name || '')}</a>
        </div>`
      ).join('') || '<span class="muted">(no edges)</span>';

      el.innerHTML = `
        <div class="detail-head">
          <span class="ntype ${esc(n.type)}">${esc(n.type)}</span>
          <span class="name">${esc(n.name)}</span>
          <span class="id">#${n.id}</span>
          ${userFlag}
          <span style="margin-left:auto">${delBtn}</span>
        </div>
        <div class="detail-meta">
          <span>content: ${fmtBytes(n.content_bytes || 0)}</span>
          <span>edges: ${(n.edges||[]).length}</span>
        </div>
        <pre class="content">${esc(n.content || '(empty)')}</pre>
        <div class="edges">
          <h3>edges →</h3>
          ${edges}
        </div>
      `;

      el.querySelectorAll('.edge-link').forEach(a => {
        a.addEventListener('click', ev => {
          ev.preventDefault();
          const tid = Number(a.dataset.id);
          selectedId = tid;
          expanded.add(tid);
          rerenderTreeLocal();
          showDetail(tid);
        });
      });

      const db = document.getElementById('del-btn');
      if (db) {
        db.addEventListener('click', async () => {
          if (!confirm('delete node #' + n.id + ' "' + n.name + '"?')) return;
          try {
            const r = await fetch('/nodes/' + n.id, { method: 'DELETE' });
            if (!r.ok) throw new Error('HTTP ' + r.status);
            flash('deleted #' + n.id, false);
            selectedId = null;
            document.getElementById('detail').innerHTML = '<span class="muted">node deleted.</span>';
            refreshTree();
          } catch (e) {
            flash(e.message, true);
          }
        });
      }
    } catch (e) {
      el.innerHTML = '<span class="muted">error: ' + esc(e.message) + '</span>';
    }
  }

  // ── Create form ───────────────────────────────────────
  function flash(text, err) {
    const m = document.getElementById('msg');
    m.textContent = text;
    m.className = err ? 'err' : '';
    setTimeout(() => { m.textContent = ''; }, 3500);
  }

  document.getElementById('create-form').addEventListener('submit', async ev => {
    ev.preventDefault();
    const name = document.getElementById('f-name').value.trim();
    const type = document.getElementById('f-type').value;
    const content = document.getElementById('f-content').value;
    if (!name) { flash('name required', true); return; }

    const body = new URLSearchParams();
    body.set('name', name);
    body.set('type', type);
    body.set('content', content);

    try {
      const r = await fetch('/nodes', {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: body.toString(),
      });
      const txt = await r.text();
      if (!r.ok) { flash('create failed: ' + r.status + ' ' + txt, true); return; }
      const obj = JSON.parse(txt);
      flash('created #' + obj.id + ' ' + obj.name, false);
      document.getElementById('f-name').value = '';
      document.getElementById('f-content').value = '';
      expanded.add(12); // ensure /user is open
      await refreshTree();
      selectedId = obj.id;
      showDetail(obj.id);
      rerenderTreeLocal();
    } catch (e) {
      flash('error: ' + e.message, true);
    }
  });

  // ── Boot ──────────────────────────────────────────────
  async function boot() {
    try {
      const ov = await jfetch('/');
      document.getElementById('version').textContent = 'v' + (ov.helios?.version || '?');
    } catch (e) {}
    await refreshStats();
    await refreshTree();
    setInterval(refreshStats, 2000);
    setInterval(refreshTree, 5000);
  }
  boot();
})();
</script>

</body>
</html>
"##;
