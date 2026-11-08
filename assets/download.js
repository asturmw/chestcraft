// chestcraft download menu, the two ways that need a script (design NOTES, round 3):
// - "Save to folder": the File System Access API (Chrome/Edge desktop), files fetched one
//   by one into a folder the visitor picks, subfolders kept. Never overwrites: a file with
//   the same name and size counts as already there, another size gets "name (2).ext".
// - "Separately": every file as its own download (?dl), every folder as its own zip; more
//   than 20 ask first.
// Both run at most 4 downloads at a time, for everyone. Browser-managed downloads can't be
// watched, so "separately" asks the server how many run (/.cc/dl-slots) before starting
// the next one. The list comes from ?tree (folder) or op=tree (selection), see web::files.
(() => {
  'use strict';
  const browser = document.querySelector('[data-js="browser"]');
  if (!browser) return;
  const ds = browser.dataset;
  const MAX = 4;
  const MANY = 20;
  const queue = document.querySelector('[data-js="queue"]');
  const qText = queue && queue.querySelector('[data-js="queue-text"]');
  const qFill = queue && queue.querySelector('[data-js="queue-fill"]');
  const qStop = queue && queue.querySelector('[data-js="queue-stop"]');
  const many = browser.querySelector('[data-js="dl-many-dialog"]');
  const canSave = typeof window.showDirectoryPicker === 'function';
  const hidden = ds.hidden === '1';

  const fill = (tpl, args) => (tpl || '').replace(/\{(\w+)\}/g, (m, k) => (k in args ? String(args[k]) : m));
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const enc = (path) => path.split('/').map(encodeURIComponent).join('/');
  const here = (rel) => new URL(rel, location.href).href;

  // Show the two script-only ways in every download menu.
  for (const menu of document.querySelectorAll('[role="menu"][data-scope]')) {
    const save = menu.querySelector('[data-js="dl-save"]');
    const each = menu.querySelector('[data-js="dl-each"]');
    if (save) save.hidden = !canSave;
    if (each) each.hidden = false;
    if (save && canSave) save.addEventListener('click', () => saveToFolder(menu.dataset.scope));
    if (each) each.addEventListener('click', () => separately(menu.dataset.scope));
  }
  // The selection's "separately" hint: how many downloads that makes.
  browser.addEventListener('cc:selection', (e) => {
    const n = e.detail.rows.length;
    const hint = document.querySelector('[data-scope="selection"] [data-js="dl-each"] [data-js="hint"]');
    if (hint) hint.textContent = fill(n === 1 ? ds.tEachOne : ds.tEachOther, { count: n });
  });

  const selectedRows = () => (window.cc && window.cc.browser ? window.cc.browser.selected() : []);

  // --- the queue toast ---------------------------------------------------------------

  let job = null;
  function show(done, total, verb) {
    if (!queue) return;
    queue.hidden = false;
    const strong = document.createElement('strong');
    strong.textContent = fill(ds.tQueueCount, { done, total });
    qText.replaceChildren(strong, ` ${verb}`);
    qFill.setAttribute('width', String(total ? Math.round((done / total) * 100) : 0));
    qStop.setAttribute('aria-label', ds.tQueueStop);
  }
  function finish(text) {
    if (!queue) return;
    queue.hidden = false;
    qText.textContent = text;
    qFill.setAttribute('width', '100');
    qStop.setAttribute('aria-label', ds.tQueueClose);
  }
  if (qStop) {
    qStop.addEventListener('click', () => {
      if (job && !job.done) {
        job.cancelled = true;
        if (job.abort) job.abort.abort();
      }
      queue.hidden = true;
    });
  }
  function start() {
    if (job && !job.done) {
      job.cancelled = true;
      if (job.abort) job.abort.abort();
    }
    job = { cancelled: false, done: false };
    return job;
  }

  // --- what to download ----------------------------------------------------------------

  // The recursive list of the folder (names = null) or of the selected names.
  async function tree(names, signal) {
    for (let attempt = 0; attempt < 30; attempt += 1) {
      let res;
      if (!names) {
        res = await fetch(here(`${location.pathname}?tree${hidden ? '&hidden=1' : ''}`), { signal, cache: 'no-store' });
      } else {
        const body = new URLSearchParams({ op: 'tree' });
        if (hidden) body.append('hidden', '1');
        for (const n of names) body.append('n', n);
        res = await fetch(here(location.pathname), { method: 'POST', body, signal });
      }
      if (res.status === 429) { await sleep(1000); continue; }
      if (!res.ok) throw new Error(`tree ${res.status}`);
      const text = await res.text();
      return text.split('\n').filter(Boolean).map((l) => JSON.parse(l));
    }
    throw new Error('tree: no free download slot');
  }

  // How many more downloads may start now (at most MAX at a time).
  async function free() {
    try {
      const r = await fetch(ds.slots, { cache: 'no-store' });
      const j = await r.json();
      if (j.max === null || j.max === undefined) return MAX - (j.active || 0);
      return Math.min(j.free, MAX - (j.max - j.free));
    } catch (e) {
      return 1;
    }
  }

  function ask(count) {
    if (!many) return Promise.resolve(true);
    many.querySelector('[data-js="dl-many-title"]').textContent = fill(ds.tMany, { count });
    many.returnValue = '';
    return new Promise((resolve) => {
      many.addEventListener('close', () => resolve(many.returnValue === 'go'), { once: true });
      many.showModal();
    });
  }

  // --- separately ------------------------------------------------------------------------

  async function separately(scope) {
    const j = start();
    let items;
    try {
      if (scope === 'selection') {
        items = selectedRows().map((r) => (r.dataset.dir
          ? `${encodeURIComponent(r.dataset.name)}/?zip${hidden ? '&hidden=1' : ''}`
          : r.dataset.dl));
      } else {
        const all = await tree(null);
        items = all.filter((e) => !e.path.includes('/'))
          .map((e) => (e.dir ? `${enc(e.path)}/?zip${hidden ? '&hidden=1' : ''}` : `${enc(e.path)}?dl`));
      }
    } catch (e) {
      j.done = true;
      finish(ds.tListFailed);
      return;
    }
    if (!items.length) { j.done = true; return; }
    if (items.length > MANY && !(await ask(items.length))) { j.done = true; return; }
    let started = 0;
    show(0, items.length, ds.tQueueStarted);
    while (started < items.length && !j.cancelled) {
      if ((await free()) <= 0) { await sleep(1000); continue; }
      if (j.cancelled) break;
      const a = document.createElement('a');
      a.href = here(items[started]);
      a.download = '';
      a.hidden = true;
      document.body.append(a);
      a.click();
      a.remove();
      started += 1;
      show(started, items.length, ds.tQueueStarted);
      // Give the request time to reach the server before counting again.
      await sleep(700);
    }
    j.done = true;
    if (!j.cancelled) setTimeout(() => { if (job === j) queue.hidden = true; }, 4000);
  }

  // --- save to folder ----------------------------------------------------------------------

  async function saveToFolder(scope) {
    let root;
    try {
      // First thing in the click: the picker needs the user's gesture.
      root = await window.showDirectoryPicker({ id: 'chestcraft', mode: 'readwrite' });
    } catch (e) {
      return; // cancelled
    }
    const j = start();
    j.abort = new AbortController();
    const signal = j.abort.signal;
    let entries;
    let base = root;
    try {
      if (scope === 'selection') {
        const names = selectedRows().map((r) => r.dataset.name);
        if (!names.length) { j.done = true; return; }
        entries = await tree(names, signal);
      } else {
        entries = await tree(null, signal);
      }
    } catch (e) {
      j.done = true;
      if (!j.cancelled) finish(ds.tListFailed);
      return;
    }
    try {
      if (scope !== 'selection') base = await root.getDirectoryHandle(ds.folder, { create: true });
    } catch (e) {
      j.done = true;
      finish(ds.tSaveDenied);
      return;
    }

    const dirs = new Map([['', base]]);
    const reserved = new Map(); // dir handle -> names taken by this run
    async function dirOf(path) {
      if (dirs.has(path)) return dirs.get(path);
      const i = path.lastIndexOf('/');
      const parent = await dirOf(i < 0 ? '' : path.slice(0, i));
      const h = await parent.getDirectoryHandle(path.slice(i + 1), { create: true });
      dirs.set(path, h);
      return h;
    }
    async function exists(dir, name) {
      const taken = reserved.get(dir);
      if (taken && taken.has(name)) return true;
      try {
        await dir.getFileHandle(name);
        return true;
      } catch (e) {
        if (e.name === 'TypeMismatchError') return true; // a folder of that name
        if (e.name === 'NotFoundError') return false;
        throw e;
      }
    }
    function reserve(dir, name) {
      if (!reserved.has(dir)) reserved.set(dir, new Set());
      reserved.get(dir).add(name);
    }
    async function freeName(dir, name) {
      const dot = name.lastIndexOf('.');
      const [stem, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ''];
      for (let n = 2; ; n += 1) {
        const candidate = `${stem} (${n})${ext}`;
        if (!(await exists(dir, candidate))) return candidate;
      }
    }

    const stats = { saved: 0, skipped: 0, renamed: 0, failed: 0 };
    const files = entries.filter((e) => !e.dir);
    try {
      for (const d of entries.filter((e) => e.dir)) await dirOf(d.path);
    } catch (e) {
      if (e.name === 'NotAllowedError' || e.name === 'SecurityError') {
        j.done = true;
        finish(ds.tSaveDenied);
        return;
      }
    }

    async function one(f) {
      const i = f.path.lastIndexOf('/');
      const dir = await dirOf(i < 0 ? '' : f.path.slice(0, i));
      let name = f.path.slice(i + 1);
      let renamed = false;
      const taken = reserved.get(dir);
      if (!(taken && taken.has(name))) {
        try {
          const old = await (await dir.getFileHandle(name)).getFile();
          if (old.size === f.size) { stats.skipped += 1; return; }
          name = await freeName(dir, name);
          renamed = true;
        } catch (e) {
          if (e.name === 'TypeMismatchError') { name = await freeName(dir, name); renamed = true; }
          else if (e.name !== 'NotFoundError') throw e;
        }
      } else {
        name = await freeName(dir, name);
        renamed = true;
      }
      reserve(dir, name);
      let res;
      for (;;) {
        res = await fetch(here(`${enc(f.path)}?dl`), { signal });
        if (res.status !== 429) break;
        await sleep(1000);
      }
      if (!res.ok) throw new Error(`download ${res.status}`);
      const fh = await dir.getFileHandle(name, { create: true });
      const w = await fh.createWritable();
      try {
        await res.body.pipeTo(w);
      } catch (e) {
        // The file is our own, new and empty: take it away again.
        try { await dir.removeEntry(name); } catch (_) { /* gone already */ }
        throw e;
      }
      stats.saved += 1;
      if (renamed) stats.renamed += 1;
    }

    let next = 0;
    let done = 0;
    show(0, files.length, ds.tQueueSaved);
    async function worker() {
      while (next < files.length && !j.cancelled) {
        const f = files[next];
        next += 1;
        try {
          await one(f);
        } catch (e) {
          if (j.cancelled) return;
          stats.failed += 1;
        }
        done += 1;
        show(done, files.length, ds.tQueueSaved);
      }
    }
    const limit = Math.max(1, Math.min(MAX, await free()));
    await Promise.all(Array.from({ length: Math.min(limit, files.length) }, worker));
    j.done = true;
    if (j.cancelled) return;
    let text = fill(ds.tSaveDone, { saved: stats.saved, skipped: stats.skipped, renamed: stats.renamed });
    if (stats.failed) text += ` · ${fill(ds.tSaveFailed, { count: stats.failed })}`;
    finish(text);
  }
})();
