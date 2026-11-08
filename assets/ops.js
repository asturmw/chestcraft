// chestcraft rename, move, copy and the in-app clipboard (copy or cut, then paste into
// another folder) over the JSON API in web/ops.rs: one item per call; a taken name asks
// keep both / replace / skip, like uploads. Accounts only (the browser element carries
// data-vol and data-csrf). The clipboard lives in localStorage, so it survives folder
// changes and works across tabs.
(() => {
  'use strict';
  const browser = document.querySelector('[data-js="browser"]');
  if (!browser || !browser.dataset.vol) return;
  const ds = browser.dataset;
  const vol = ds.vol;
  const dir = ds.dir || '';
  const B = window.cc.browser;
  const $ = (sel, root = browser) => root.querySelector(`[data-js="${sel}"]`);
  const $$ = (sel, root = browser) => [...root.querySelectorAll(`[data-js="${sel}"]`)];
  const fill = (tpl, args) => (tpl || '').replace(/\{(\w+)\}/g, (m, k) => (k in args ? String(args[k]) : m));
  const join = (d, n) => (d ? `${d}/${n}` : n);
  const enc = (path) => path.split('/').map(encodeURIComponent).join('/');
  const STORE = 'cc-clipboard';
  const FLASH = 'cc-flash';

  async function api(op, body) {
    let r;
    try {
      r = await fetch(`/.cc/${op}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-csrf-token': ds.csrf },
        body: JSON.stringify(body),
      });
    } catch (e) {
      return { ok: false, status: 0, data: null };
    }
    let data = null;
    try { data = await r.json(); } catch (e) { /* empty body */ }
    return { ok: r.ok, status: r.status, data };
  }

  // --- toast (the download queue's element) -----------------------------------------

  const queue = document.querySelector('[data-js="queue"]');
  function toast(text, part) {
    if (!queue) return;
    queue.hidden = false;
    queue.querySelector('[data-js="queue-text"]').textContent = text;
    queue.querySelector('[data-js="queue-fill"]').setAttribute('width', String(part === undefined ? 100 : Math.round(part * 100)));
    queue.querySelector('[data-js="queue-stop"]').setAttribute('aria-label', ds.tQueueClose);
  }
  // A message from before the reload.
  try {
    const flash = sessionStorage.getItem(FLASH);
    if (flash) { sessionStorage.removeItem(FLASH); toast(flash); }
  } catch (e) { /* storage off */ }

  // --- the clipboard -----------------------------------------------------------------

  function load() {
    try {
      const c = JSON.parse(localStorage.getItem(STORE));
      if (c && (c.mode === 'copy' || c.mode === 'cut') && Array.isArray(c.items) && c.items.length) return c;
    } catch (e) { /* none */ }
    return null;
  }
  function save(c) {
    try {
      if (c) localStorage.setItem(STORE, JSON.stringify(c));
      else localStorage.removeItem(STORE);
    } catch (e) { /* storage off: the chip stays empty */ }
    chip();
  }
  function put(mode, rows) {
    if (!rows.length) return;
    save({ mode, items: rows.map((r) => ({ vol, path: join(dir, r.dataset.name), name: r.dataset.name })) });
  }
  const paste = $('paste');
  function chip() {
    if (!paste) return;
    const c = load();
    paste.hidden = !c;
    if (!c) return;
    const n = c.items.length;
    $('paste-count', paste).textContent = String(n);
    $('paste-go', paste).setAttribute('aria-label', fill(n === 1 ? ds.tPasteOne : ds.tPasteOther, { count: n }));
    $('clip-clear', paste).setAttribute('aria-label', fill(c.mode === 'cut' ? ds.tClipCut : ds.tClipCopy, { count: n }));
  }
  addEventListener('storage', (e) => { if (e.key === STORE) chip(); });
  if (paste) {
    $('paste-go', paste).addEventListener('click', () => pasteHere());
    $('clip-clear', paste).addEventListener('click', () => save(null));
  }
  chip();

  // --- name clashes --------------------------------------------------------------------

  const clashDialog = document.querySelector('[data-js="op-clash"]');
  function askClash(data, others) {
    return new Promise((resolve) => {
      const q = (s) => $(s, clashDialog);
      q('clash-title').textContent = data.message || '';
      q('clash-existing').textContent = (data.clash && data.clash.existing_text) || '';
      q('clash-replace').hidden = !(data.clash && data.clash.can_replace);
      q('clash-all-label').hidden = others === 0;
      const text = q('clash-all-text');
      text.textContent = fill(text.dataset.t, { count: others });
      const box = q('clash-all');
      box.checked = false;
      clashDialog.returnValue = '';
      clashDialog.addEventListener('close', () => {
        const v = ['keep_both', 'replace', 'skip'].includes(clashDialog.returnValue) ? clashDialog.returnValue : null;
        resolve({ value: v, all: box.checked });
      }, { once: true });
      clashDialog.showModal();
    });
  }

  // Move or copy items one by one into `to` ({vol, dir}); returns what happened.
  async function run(kind, items, to) {
    let forAll = null;
    let done = 0;
    let changed = 0;
    let cancelled = false;
    const fails = [];
    const verb = kind === 'move' ? ds.tMoved : ds.tCopied;
    for (const it of items) {
      if (cancelled) break;
      let clash = forAll || 'ask';
      for (;;) {
        const r = await api(kind, { vol: it.vol, path: it.path, to_vol: to.vol, to_dir: to.dir, clash });
        if (r.status === 409 && r.data && r.data.clash) {
          const choice = await askClash(r.data, items.length - done - 1);
          if (!choice.value) { cancelled = true; break; }
          if (choice.all) forAll = choice.value;
          clash = choice.value;
          continue;
        }
        if (r.ok) {
          if (!(r.data && r.data.skipped)) changed += 1;
        } else {
          fails.push(`${it.name}: ${(r.data && r.data.message) || r.status}`);
        }
        break;
      }
      done += 1;
      toast(`${fill(ds.tQueueCount, { done, total: items.length })} ${verb}`, done / items.length);
    }
    return { changed, fails, done };
  }

  function finish(res, kind) {
    const verb = kind === 'move' ? ds.tMoved : ds.tCopied;
    let text = `${fill(ds.tQueueCount, { done: res.changed, total: res.done })} ${verb}`;
    if (res.fails.length) text += ` · ${res.fails[0]}${res.fails.length > 1 ? ` (+${res.fails.length - 1})` : ''}`;
    if (res.changed) {
      try { sessionStorage.setItem(FLASH, text); } catch (e) { /* no flash then */ }
      location.reload();
    } else {
      toast(text);
    }
  }

  async function pasteHere() {
    const c = load();
    if (!c || ds.canWrite !== '1') return;
    // Cut items already in this folder stay where they are.
    const items = c.mode === 'cut'
      ? c.items.filter((it) => !(it.vol === vol && it.path === join(dir, it.name)))
      : c.items;
    if (!items.length) { save(null); return; }
    const kind = c.mode === 'cut' ? 'move' : 'copy';
    const res = await run(kind, items, { vol, dir });
    if (kind === 'move' && res.changed) save(null);
    finish(res, kind);
  }

  // --- rename ----------------------------------------------------------------------------

  const renameDialog = $('rename-dialog');
  function rename(row) {
    if (!row || !renameDialog || ds.canMove !== '1') return;
    const input = renameDialog.querySelector('input');
    const field = $('rename-field', renameDialog);
    const error = $('rename-error', renameDialog);
    const name = row.dataset.name;
    delete field.dataset.state;
    error.textContent = '';
    input.value = name;
    renameDialog.returnValue = '';
    renameDialog.showModal();
    input.focus();
    const dot = name.lastIndexOf('.');
    input.setSelectionRange(0, !row.dataset.dir && dot > 0 ? dot : name.length);
    const form = renameDialog.querySelector('form');
    form.onsubmit = async (e) => {
      e.preventDefault();
      const to = input.value.trim();
      if (!to || to === name) { renameDialog.close(); return; }
      const r = await api('rename', { vol, path: join(dir, name), name: to, clash: 'ask' });
      if (r.ok) {
        renameDialog.close();
        location.reload();
        return;
      }
      field.dataset.state = 'error';
      error.textContent = (r.data && r.data.message) || String(r.status);
      input.focus();
    };
  }

  // --- move and copy: the folder picker ------------------------------------------------------

  const picker = $('picker');
  async function choose(kind, rows) {
    if (!picker || !rows.length) return;
    const title = rows.length === 1
      ? fill(kind === 'move' ? ds.tMoveOne : ds.tCopyOne, { name: rows[0].dataset.name })
      : fill(kind === 'move' ? ds.tMoveOther : ds.tCopyOther, { count: rows.length });
    $('picker-title', picker).textContent = title;
    const go = $('picker-go', picker);
    go.textContent = kind === 'move' ? ds.tMoveHere : ds.tCopyHere;
    const sources = new Set(rows.map((r) => join(dir, r.dataset.name)));
    let at = { vol, dir };

    async function show(place) {
      at = place;
      const url = place.vol === null ? '/?dirs' : `/${encodeURIComponent(place.vol)}/${place.dir ? `${enc(place.dir)}/` : ''}?dirs`;
      let data = { dirs: [], write: false, move: false };
      try {
        const r = await fetch(url, { cache: 'no-store' });
        if (r.ok) data = await r.json();
      } catch (e) { /* stays empty */ }
      // The path: all volumes, then the folders down to here.
      const path = $('picker-path', picker);
      const crumbs = [{ label: ds.tVolumes, place: { vol: null, dir: '' } }];
      if (place.vol !== null) {
        crumbs.push({ label: place.vol, place: { vol: place.vol, dir: '' } });
        let acc = '';
        for (const part of place.dir ? place.dir.split('/') : []) {
          acc = join(acc, part);
          crumbs.push({ label: part, place: { vol: place.vol, dir: acc } });
        }
      }
      path.replaceChildren(...crumbs.map((c, i) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'picker__crumb';
        b.textContent = c.label;
        if (i === crumbs.length - 1) b.setAttribute('aria-current', 'location');
        b.addEventListener('click', () => show(c.place));
        return b;
      }));
      const list = $('picker-list', picker);
      const entries = data.dirs.filter((name) => {
        // A folder can't go into itself.
        if (kind !== 'move' || place.vol !== vol) return true;
        return !sources.has(join(place.dir, name));
      });
      list.replaceChildren(...entries.map((name) => {
        const li = document.createElement('li');
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'picker__item';
        b.append(window.cc.icon(place.vol === null ? 'drive' : 'folder'), document.createTextNode(name));
        b.addEventListener('click', () => show(place.vol === null ? { vol: name, dir: '' } : { vol: place.vol, dir: join(place.dir, name) }));
        li.append(b);
        return li;
      }));
      $('picker-note', picker).textContent = entries.length ? '' : ds.tPickerEmpty;
      const allowed = place.vol !== null
        && (kind === 'copy' ? data.write : (place.vol === vol ? data.move : data.write))
        && !(kind === 'move' && place.vol === vol && place.dir === dir);
      go.disabled = !allowed;
    }

    picker.returnValue = '';
    await show(at);
    picker.showModal();
    const closed = await new Promise((r) => picker.addEventListener('close', () => r(picker.returnValue), { once: true }));
    if (closed !== 'go' || at.vol === null) return;
    const items = rows.map((r) => ({ vol, path: join(dir, r.dataset.name), name: r.dataset.name }));
    finish(await run(kind, items, { vol: at.vol, dir: at.dir }), kind);
  }

  // --- wiring ----------------------------------------------------------------------------------

  const targets = () => B.selected();
  for (const b of $$('move')) b.addEventListener('click', () => choose('move', targets()));
  for (const b of $$('copy')) b.addEventListener('click', () => choose('copy', targets()));
  for (const b of $$('rename')) b.addEventListener('click', () => { const t = targets(); if (t.length === 1) rename(t[0]); });
  // Rename needs exactly one item.
  browser.addEventListener('cc:selection', (e) => {
    for (const b of $$('rename')) b.hidden = e.detail.rows.length !== 1;
  });
  browser.addEventListener('cc:row-menu', (e) => {
    const { row, add } = e.detail;
    if (ds.canMove === '1') {
      add(ds.tRename, 'rename', () => rename(row));
      add(ds.tMove, 'move', () => choose('move', [row]));
    }
    add(ds.tCopy, 'copy', () => choose('copy', [row]));
  });
  B.update();

  window.cc.ops = {
    copy: (rows) => put('copy', rows),
    cut: (rows) => { if (ds.canMove === '1') put('cut', rows); },
    paste: pasteHere,
    rename,
    move: (rows) => choose('move', rows),
    copyTo: (rows) => choose('copy', rows),
  };
})();
