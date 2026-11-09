// chestcraft upload client: chunked, parallel, resumable uploads with name-clash decisions.
// Protocol: src/web/api.rs. Hooks are data-js attributes rendered by src/web/views.rs;
// state goes into data-state, copy comes from data-t-* attributes (the i18n catalogs).
(() => {
  'use strict';
  const root = document.querySelector('[data-js="upload"]');
  if (!root) return;
  const tray = document.querySelector('[data-js="upload-tray"]');
  const list = tray.querySelector('[data-js="upload-list"]');
  const summary = tray.querySelector('[data-js="upload-summary"]');
  const dialog = document.querySelector('[data-js="name-clash"]');
  const q = (sel) => dialog.querySelector(`[data-js="${sel}"]`);
  const ds = root.dataset;
  const vol = ds.vol;
  // /.cc, or /s/<id>/.cc for share visitors (dir is then relative to the share).
  const base = ds.api || '/.cc';
  const dir = ds.dir;
  const csrf = ds.csrf;
  const parallel = Math.max(1, parseInt(ds.parallel, 10) || 3);
  const T = {
    queued: ds.tQueued, uploading: ds.tUploading, done: ds.tDone, failed: ds.tFailed,
    skipped: ds.tSkipped, clash: ds.tClash, savedAs: ds.tSavedAs, reload: ds.tReload,
    progress: ds.tProgress, rate: ds.tRate, left: ds.tLeft, unfinished: ds.tUnfinished,
    discard: ds.tDiscard, uploadedOne: ds.tUploadedOne, uploadedOther: ds.tUploadedOther,
  };
  const fill = (tpl, args) => (tpl || '').replace(/\{(\w+)\}/g, (m, k) => (k in args ? String(args[k]) : m));
  // Same as the server's sizes (SI units, the language's decimal mark).
  function sizeText(bytes) {
    const units = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'];
    if (bytes < 1000) return `${Math.round(bytes)} B`;
    let v = bytes;
    let u = 0;
    while (v >= 1000 && u < units.length - 1) { v /= 1000; u += 1; }
    const t = v < 100 ? v.toFixed(1) : v.toFixed(0);
    return `${t.replace('.', ds.decimal || '.')} ${units[u]}`;
  }
  function timeText(secs) {
    const s = Math.max(1, Math.round(secs));
    if (s < 60) return `${s} s`;
    if (s < 3600) return `${Math.round(s / 60)} min`;
    return `${Math.floor(s / 3600)} h ${Math.round((s % 3600) / 60)} min`;
  }
  // Files started (a .part exists on the server) but not finished.
  const MAX_ACTIVE = 8;
  const STORE = 'cc-upload:';

  const items = [];
  const queue = [];
  const chunks = [];
  let active = 0;
  let workers = 0;
  let starting = false;
  let wake = null;
  let ended = 0;
  let totalBytes = 0;
  let sentBytes = 0;
  // Upload speed: bytes per second, smoothed over one-second samples. Chunks a resumed
  // upload already had don't count.
  let rate = 0;
  let rateAt = 0;
  let rateBytes = 0;
  let clashForAll = null;
  let dialogChain = Promise.resolve();

  // On folder pages the action bar's Upload button opens the file picker.
  if (ds.pick !== 'bar') root.hidden = false;

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const store = {
    get(k) { try { return localStorage.getItem(STORE + k); } catch (e) { return null; } },
    set(k, v) { try { localStorage.setItem(STORE + k, v); } catch (e) { /* private mode */ } },
    del(k) { try { localStorage.removeItem(STORE + k); } catch (e) { /* private mode */ } },
  };

  async function api(method, url, body) {
    const headers = { 'X-CSRF-Token': csrf };
    const init = { method, headers, credentials: 'same-origin' };
    if (body !== undefined) {
      headers['Content-Type'] = 'application/json';
      init.body = JSON.stringify(body);
    }
    let res;
    try {
      res = await fetch(url, init);
    } catch (e) {
      return { status: 0, ok: false, data: null };
    }
    let data = null;
    try { data = await res.json(); } catch (e) { data = null; }
    return { status: res.status, ok: res.ok, data };
  }

  const uploadUrl = (item, tail) => `${base}/upload/${encodeURIComponent(item.id)}${tail || ''}`;

  function sampleRate() {
    const now = performance.now();
    if (!rateAt) { rateAt = now; rateBytes = sentBytes; return; }
    const dt = (now - rateAt) / 1000;
    if (dt < 1) return;
    const r = Math.max(0, (sentBytes - rateBytes) / dt);
    rate = rate ? rate * 0.6 + r * 0.4 : r;
    rateAt = now;
    rateBytes = sentBytes;
  }

  function updateSummary() {
    const pct = totalBytes ? Math.floor((sentBytes / totalBytes) * 100) : 100;
    const busy = ended < items.length;
    const parts = [`${ended} / ${items.length}`, `${fill(T.progress, { sent: sizeText(sentBytes), total: sizeText(totalBytes) })} (${Math.min(pct, 100)} %)`];
    if (busy) {
      sampleRate();
      if (rate > 0) {
        parts.push(fill(T.rate, { rate: sizeText(rate) }));
        const left = (totalBytes - sentBytes) / rate;
        if (left > 0 && Number.isFinite(left)) parts.push(fill(T.left, { time: timeText(left) }));
      }
    } else {
      rate = 0;
      rateAt = 0;
    }
    summary.textContent = parts.join(' · ');
    tray.dataset.state = busy ? 'active' : 'done';
    if (!busy) whenAllEnded();
  }
  // The speed keeps moving while chunks are on their way.
  setInterval(() => { if (ended < items.length) updateSummary(); }, 1000);

  // Everything ended: without failures the listing reloads with the new files (and says
  // how many arrived); otherwise the tray stays, with a button to show the new files.
  let reloadTimer = null;
  function whenAllEnded() {
    if (!items.length) return;
    const failed = items.some((i) => i.li.dataset.state === 'failed');
    const done = items.filter((i) => i.li.dataset.state === 'done').length;
    if (!failed && done) {
      if (reloadTimer) return;
      const count = items.length;
      reloadTimer = setTimeout(() => {
        reloadTimer = null;
        if (ended !== items.length || items.length !== count) return;
        const text = fill(done === 1 ? T.uploadedOne : T.uploadedOther, { count: done });
        if (window.cc && window.cc.reloadWith) window.cc.reloadWith(text);
        else window.location.reload();
      }, 700);
      return;
    }
    if (!tray.querySelector('[data-js="upload-reload"]')) {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'upload-tray__reload';
      b.dataset.js = 'upload-reload';
      b.textContent = T.reload;
      b.addEventListener('click', () => window.location.reload());
      tray.append(b);
    }
  }

  function setState(item, state, text) {
    item.li.dataset.state = state;
    item.stateEl.textContent = text || T[state] || state;
    updateSummary();
  }

  function addSent(item, n, resumed) {
    item.sent += n;
    sentBytes += n;
    if (resumed) rateBytes += n;
    item.prog.value = Math.min(item.sent, item.prog.max);
  }

  function addItem(file, name, batch) {
    const li = document.createElement('li');
    li.className = 'upload-item';
    const label = document.createElement('span');
    label.className = 'upload-item__name';
    label.textContent = name;
    const prog = document.createElement('progress');
    prog.className = 'upload-item__progress';
    prog.max = Math.max(file.size, 1);
    prog.value = 0;
    const stateEl = document.createElement('span');
    stateEl.className = 'upload-item__state';
    li.append(label, prog, stateEl);
    list.append(li);
    const item = {
      file, name, batch, li, prog, stateEl,
      id: null, key: null, chunk: 0, pending: 0, sent: 0, active: false, over: false,
    };
    items.push(item);
    totalBytes += file.size;
    setState(item, 'queued');
    return item;
  }

  // An item is over: done, skipped or failed.
  function end(item, state, text) {
    if (item.over) return;
    item.over = true;
    ended += 1;
    if (item.key) store.del(item.key);
    if (state === 'done') {
      item.prog.value = item.prog.max;
    } else {
      // What wasn't sent won't be; keep the overall percentage honest.
      totalBytes -= item.file.size - item.sent;
    }
    setState(item, state, text);
    if (item.active) {
      item.active = false;
      active -= 1;
      if (wake) { const w = wake; wake = null; w(); }
    }
    if (item.batch) {
      item.batch.open -= 1;
      if (item.batch.open === 0) finishBatch(item.batch);
    }
  }

  function fail(item, res) {
    const msg = res && res.data && res.data.message;
    for (let i = chunks.length - 1; i >= 0; i -= 1) {
      if (chunks[i].item === item) chunks.splice(i, 1);
    }
    // Free the server side (and an open folder batch); a new attempt starts over.
    if (item.id) api('DELETE', uploadUrl(item));
    end(item, 'failed', msg ? `${T.failed}: ${msg}` : T.failed);
  }

  async function finishBatch(batch) {
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const r = await api('POST', `${base}/batch/${encodeURIComponent(batch.id)}/done`);
      if (r.ok || (r.status >= 400 && r.status < 500 && r.status !== 409)) return;
      await sleep(1000 * (attempt + 1));
    }
  }

  // The user's choice for a taken name: "rename" (keep both), "replace" or "skip".
  function decide(item, data) {
    const pick = () => {
      if (clashForAll) {
        return Promise.resolve(
          clashForAll === 'replace' && !data.clash.can_replace ? 'rename' : clashForAll,
        );
      }
      return ask(item, data);
    };
    const next = dialogChain.then(pick);
    dialogChain = next.catch(() => {});
    return next;
  }

  function ask(item, data) {
    return new Promise((resolve) => {
      setState(item, 'clash');
      q('clash-title').textContent = data.message;
      q('clash-existing').textContent = data.clash.existing_text || '';
      q('clash-new').textContent = data.clash.new_text || '';
      q('clash-replace').hidden = !data.clash.can_replace;
      const others = queue.length + Math.max(active - 1, 0);
      const box = q('clash-all');
      const text = q('clash-all-text');
      q('clash-all-label').hidden = others === 0;
      text.textContent = (text.dataset.t || '').replace('{count}', String(others));
      box.checked = false;
      dialog.returnValue = '';
      dialog.addEventListener('close', () => {
        const v = ['rename', 'replace', 'skip'].includes(dialog.returnValue) ? dialog.returnValue : 'skip';
        if (box.checked) clashForAll = v;
        setState(item, 'uploading');
        resolve(v);
      }, { once: true });
      dialog.showModal();
    });
  }

  async function start(item) {
    item.key = [vol, dir, item.name, item.file.size, item.file.lastModified].join('|');
    let status = null;
    const saved = savedId(store.get(item.key));
    const notice = notices.get(item.key);
    if (notice) { notices.delete(item.key); notice.remove(); }
    if (saved) {
      item.id = saved;
      const r = await api('GET', uploadUrl(item));
      if (r.ok && r.data && r.data.size === item.file.size) status = r.data;
      else { item.id = null; store.del(item.key); }
    }
    let clash = clashForAll || 'ask';
    while (!status) {
      const body = {
        vol, dir, name: item.name, size: item.file.size, mtime: item.file.lastModified, clash,
      };
      if (item.batch) body.batch = item.batch.id;
      const r = await api('POST', `${base}/upload`, body);
      if (r.status === 201) {
        status = r.data;
        store.set(item.key, JSON.stringify({ id: status.id, vol, dir, name: item.name, size: item.file.size }));
      } else if (r.ok && r.data && r.data.skipped) {
        end(item, 'skipped');
        return;
      } else if (r.status === 409 && r.data && r.data.clash) {
        clash = await decide(item, r.data);
      } else {
        fail(item, r);
        return;
      }
    }
    item.id = status.id;
    item.chunk = status.chunk;
    item.active = true;
    active += 1;
    const have = new Set(status.have);
    for (let n = 0; n < status.chunks; n += 1) {
      if (have.has(n)) {
        addSent(item, chunkEnd(item, n) - n * item.chunk, true);
      } else {
        chunks.push({ item, n });
        item.pending += 1;
      }
    }
    setState(item, 'uploading');
    if (item.pending === 0) finish(item);
    runWorkers();
  }

  const chunkEnd = (item, n) => Math.min((n + 1) * item.chunk, item.file.size);

  async function starter() {
    if (starting) return;
    starting = true;
    try {
      while (queue.length) {
        if (active >= MAX_ACTIVE) {
          await new Promise((r) => { wake = r; });
          continue;
        }
        await start(queue.shift());
      }
    } finally {
      starting = false;
    }
  }

  function runWorkers() {
    while (workers < parallel && chunks.length) {
      workers += 1;
      worker().finally(() => {
        workers -= 1;
        runWorkers();
      });
    }
  }

  async function worker() {
    while (chunks.length) {
      const { item, n } = chunks.shift();
      if (item.over) continue;
      const res = await sendChunk(item, n);
      if (item.over) continue;
      if (res !== true) {
        fail(item, res);
        continue;
      }
      item.pending -= 1;
      if (item.pending === 0) finish(item);
    }
  }

  async function sendChunk(item, n) {
    const from = n * item.chunk;
    const to = chunkEnd(item, n);
    const blob = item.file.slice(from, to);
    let last = null;
    for (let attempt = 0; attempt < 6; attempt += 1) {
      if (attempt) await sleep(1000 * 2 ** (attempt - 1));
      last = await putChunk(item, n, blob, to - from);
      if (last === true) return true;
      // Retry network errors, server errors and "busy"; give up on everything else.
      if (last.status && last.status < 500 && last.status !== 409 && last.status !== 429) {
        return last;
      }
    }
    return last;
  }

  function putChunk(item, n, blob, len) {
    return new Promise((resolve) => {
      const xhr = new XMLHttpRequest();
      let counted = 0;
      const undo = () => { addSent(item, -counted); counted = 0; };
      xhr.open('PUT', uploadUrl(item, `/chunk/${n}`));
      xhr.setRequestHeader('X-CSRF-Token', csrf);
      xhr.setRequestHeader('Content-Type', 'application/octet-stream');
      xhr.upload.onprogress = (e) => {
        addSent(item, e.loaded - counted);
        counted = e.loaded;
        updateSummary();
      };
      xhr.onload = () => {
        if (xhr.status === 200) {
          addSent(item, len - counted);
          updateSummary();
          resolve(true);
          return;
        }
        undo();
        let data = null;
        try { data = JSON.parse(xhr.responseText); } catch (e) { data = null; }
        resolve({ status: xhr.status, ok: false, data });
      };
      xhr.onerror = () => { undo(); resolve({ status: 0, ok: false, data: null }); };
      xhr.send(blob);
    });
  }

  async function finish(item) {
    let clash;
    for (let attempt = 0; attempt < 8; attempt += 1) {
      const r = await api('POST', uploadUrl(item, '/done'), clash ? { clash } : {});
      if (r.ok && r.data && r.data.skipped) {
        end(item, 'skipped');
        return;
      }
      if (r.ok) {
        end(item, 'done', r.data && r.data.renamed
          ? T.savedAs.replace('{name}', r.data.path) : T.done);
        return;
      }
      if (r.status === 409 && r.data && r.data.clash) {
        clash = await decide(item, r.data);
        attempt = -1;
      } else if (r.status === 0 || r.status >= 500 || (r.data && r.data.error === 'busy')) {
        await sleep(1000 * (attempt + 1));
      } else {
        fail(item, r);
        return;
      }
    }
    fail(item, null);
  }

  async function enqueue(entries) {
    if (!entries.length) return;
    tray.hidden = false;
    let batch = null;
    if (entries.some((e) => e.name.includes('/'))) {
      const r = await api('POST', `${base}/batch`, { vol, dir });
      if (r.ok && r.data) batch = { id: r.data.id, open: entries.length };
    }
    for (const e of entries) queue.push(addItem(e.file, e.name, batch));
    starter();
  }

  // --- uploads that stopped -------------------------------------------------------------
  // A closed tab or a lost connection leaves a started upload on the server. The browser
  // can't reach the file again by itself, so the tray lists it: adding the same file again
  // continues it (start() finds its id), Discard drops it.

  // Stored values: {"id","vol","dir","name","size"} as JSON, or a bare id (older pages).
  function savedId(v) {
    if (!v) return null;
    try { const o = JSON.parse(v); if (o && typeof o.id === 'string') return o.id; } catch (e) { /* bare id */ }
    return v;
  }
  const notices = new Map();
  async function showStopped() {
    let keys = [];
    try { keys = Object.keys(localStorage).filter((k) => k.startsWith(STORE)); } catch (e) { return; }
    for (const full of keys) {
      const key = full.slice(STORE.length);
      let o = null;
      try { o = JSON.parse(localStorage.getItem(full)); } catch (e) { o = null; }
      if (!o || typeof o.id !== 'string' || o.vol !== vol || o.dir !== dir) continue;
      const r = await api('GET', `${base}/upload/${encodeURIComponent(o.id)}`);
      if (!r.ok || !r.data) {
        if (r.status >= 400 && r.status < 500) store.del(key);
        continue;
      }
      if (items.some((i) => i.key === key)) continue;
      const d = r.data;
      const have = (d.have || []).reduce((n, c) => n + Math.min(d.chunk, d.size - c * d.chunk), 0);
      const li = document.createElement('li');
      li.className = 'upload-item';
      li.dataset.state = 'stopped';
      const label = document.createElement('span');
      label.className = 'upload-item__name';
      label.textContent = o.name;
      const prog = document.createElement('progress');
      prog.className = 'upload-item__progress';
      prog.max = Math.max(d.size, 1);
      prog.value = have;
      const stateEl = document.createElement('span');
      stateEl.className = 'upload-item__state';
      stateEl.textContent = fill(T.unfinished, { percent: Math.floor((have / Math.max(d.size, 1)) * 100) });
      const drop = document.createElement('button');
      drop.type = 'button';
      drop.className = 'btn upload-item__discard';
      drop.textContent = T.discard;
      drop.addEventListener('click', async () => {
        await api('DELETE', `${base}/upload/${encodeURIComponent(o.id)}`);
        store.del(key);
        notices.delete(key);
        li.remove();
        if (!list.children.length && !items.length) tray.hidden = true;
      });
      li.append(label, prog, stateEl, drop);
      list.append(li);
      notices.set(key, li);
      tray.hidden = false;
    }
  }
  showStopped();

  // --- choosing files ----------------------------------------------------------------

  const files = root.querySelector('[data-js="upload-files"]');
  const folder = root.querySelector('[data-js="upload-folder"]');
  const pick = document.querySelector('[data-js="upload-pick"]');
  if (pick) pick.addEventListener('click', () => files.click());
  files.addEventListener('change', () => {
    enqueue([...files.files].map((f) => ({ file: f, name: f.name })));
    files.value = '';
  });
  folder.addEventListener('change', () => {
    enqueue([...folder.files].map((f) => ({ file: f, name: f.webkitRelativePath || f.name })));
    folder.value = '';
  });

  // --- drag and drop anywhere on the page ----------------------------------------------

  const carriesFiles = (e) => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
  let depth = 0;
  document.addEventListener('dragenter', (e) => {
    if (!carriesFiles(e)) return;
    depth += 1;
    root.dataset.state = 'dragging';
  });
  document.addEventListener('dragleave', (e) => {
    if (!carriesFiles(e)) return;
    depth -= 1;
    if (depth <= 0) { depth = 0; delete root.dataset.state; }
  });
  document.addEventListener('dragover', (e) => {
    if (!carriesFiles(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
  });

  function readEntries(reader) {
    return new Promise((res, rej) => reader.readEntries(res, rej));
  }

  async function walk(entry, prefix, out) {
    if (entry.isFile) {
      const file = await new Promise((res, rej) => entry.file(res, rej));
      out.push({ file, name: prefix + entry.name });
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      for (;;) {
        const batch = await readEntries(reader);
        if (!batch.length) break;
        for (const child of batch) await walk(child, `${prefix}${entry.name}/`, out);
      }
    }
  }

  document.addEventListener('drop', async (e) => {
    if (!carriesFiles(e)) return;
    e.preventDefault();
    depth = 0;
    delete root.dataset.state;
    // Entries and files must be taken before the first await.
    const entries = [...e.dataTransfer.items]
      .filter((i) => i.kind === 'file')
      .map((i) => (i.webkitGetAsEntry ? i.webkitGetAsEntry() : null));
    const plain = [...e.dataTransfer.files];
    const out = [];
    if (entries.length && entries.every(Boolean)) {
      for (const en of entries) {
        try { await walk(en, '', out); } catch (err) { /* unreadable entry */ }
      }
    } else {
      for (const f of plain) out.push({ file: f, name: f.name });
    }
    enqueue(out);
  });

  window.addEventListener('beforeunload', (e) => {
    if (ended < items.length) {
      e.preventDefault();
      e.returnValue = '';
    }
  });
})();
