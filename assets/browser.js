// chestcraft folder browser: selection (select mode on phones) and the status bar, the
// filter, the row menu, new folder, delete to trash, the selection's zip and folder
// sizes. Markup: views::listing (src/web/views.rs);
// copy comes from data-t-* attributes on the browser element (the i18n catalogs). The
// selection posts through the `sel` form (op=zip | delete), like it would without JS.
(() => {
  'use strict';
  const browser = document.querySelector('[data-js="browser"]');
  if (!browser) return;
  const ds = browser.dataset;
  const $ = (sel, root = browser) => root.querySelector(`[data-js="${sel}"]`);
  const status = $('status');
  const form = $('sel-form');
  const op = $('sel-op');
  const all = $('select-all');
  // The filter: in the title row (visitors), or the app bar's search field and its phone
  // copy in the title row (accounts), so possibly two inputs that type together.
  const filters = [...document.querySelectorAll('[data-js~="filter"]')];
  const rowMenu = $('row-menu');
  const rows = [...browser.querySelectorAll('[data-js="row"]')];
  const pick = (row) => row.querySelector('[data-js="pick"]');

  const fill = (tpl, args) => tpl.replace(/\{(\w+)\}/g, (m, k) => (k in args ? args[k] : m));

  // Same as the server's sizes (SI units, the language's decimal mark).
  function sizeText(bytes) {
    const units = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'];
    if (bytes < 1000) return `${bytes} B`;
    let v = bytes;
    let u = 0;
    while (v >= 1000 && u < units.length - 1) { v /= 1000; u += 1; }
    const t = v < 100 ? v.toFixed(1) : v.toFixed(0);
    return `${t.replace('.', ds.decimal || '.')} ${units[u]}`;
  }
  const items = (n) => fill(n === 1 ? ds.tItemsOne : ds.tItemsOther, { count: String(n) });

  function setState(row, key, on) {
    const set = new Set((row.dataset.state || '').split(' ').filter(Boolean));
    if (on) set.add(key); else set.delete(key);
    if (set.size) row.dataset.state = [...set].join(' ');
    else delete row.dataset.state;
  }

  const selected = () => rows.filter((r) => pick(r).checked);
  const selecting = () => rows.some((r) => pick(r).checked);
  const visible = () => rows.filter((r) => !r.hidden);

  // What the selection's actions and its download menu show.
  function update() {
    const sel = selected();
    for (const r of rows) setState(r, 'selected', pick(r).checked);
    if (status) status.dataset.state = sel.length ? 'select' : 'idle';
    // Select mode (phones): while something is selected, the checkboxes show and taps
    // toggle rows instead of opening them; the last item unselected ends it.
    setState(browser, 'select-mode', sel.length > 0);
    // Folders count once their size is known (?du below).
    const sized = (r) => !r.dataset.dir || r.dataset.du === '1';
    const bytes = sel.reduce((n, r) => n + (sized(r) ? Number(r.dataset.size) || 0 : 0), 0);
    const files = sel.some(sized);
    const count = $('sel-count');
    if (count) count.textContent = String(sel.length);
    const size = $('sel-size');
    if (size) size.textContent = files ? `, ${sizeText(bytes)}` : '';
    const share = $('share');
    if (share) share.hidden = !(sel.length === 1 && sel[0].dataset.share);
    const zipHint = document.querySelector('#dl-sel [data-js="dl-zip"] [data-js="hint"]');
    if (zipHint) zipHint.textContent = files ? `${items(sel.length)}, ${sizeText(bytes)}` : items(sel.length);
    if (all) {
      const vis = visible();
      const on = vis.filter((r) => pick(r).checked).length;
      all.checked = on > 0 && on === vis.length;
      all.indeterminate = on > 0 && on < vis.length;
    }
    browser.dispatchEvent(new CustomEvent('cc:selection', { detail: { rows: sel } }));
  }

  function selectOnly(list) {
    for (const r of rows) pick(r).checked = list.includes(r);
    update();
  }

  let last = null; // anchor for shift ranges
  function toggle(row, range) {
    if (range && last && last !== row) {
      const vis = visible();
      const [a, b] = [vis.indexOf(last), vis.indexOf(row)].sort((x, y) => x - y);
      if (a >= 0) {
        const on = pick(last).checked;
        for (const r of vis.slice(a, b + 1)) pick(r).checked = on;
        update();
        return;
      }
    }
    pick(row).checked = !pick(row).checked;
    last = row;
    update();
  }

  for (const r of rows) {
    pick(r).addEventListener('click', (e) => {
      if (e.shiftKey && last && last !== r) {
        e.preventDefault();
        pick(r).checked = !pick(r).checked; // undo the browser's own toggle, toggle() redoes it
        toggle(r, true);
        return;
      }
      last = r;
      update();
    });
    // Phones: hold a row to select it, a shortcut for "Select" in its menu; both start
    // select mode.
    let timer = null;
    let held = false;
    r.addEventListener('touchstart', () => {
      held = false;
      timer = setTimeout(() => {
        timer = null;
        held = true;
        pick(r).checked = true;
        last = r;
        update();
      }, 500);
    }, { passive: true });
    const cancel = () => { if (timer) { clearTimeout(timer); timer = null; } };
    r.addEventListener('touchend', cancel);
    r.addEventListener('touchmove', cancel, { passive: true });
    r.addEventListener('contextmenu', (e) => { if (held) e.preventDefault(); });
    const link = r.querySelector('[data-js="row-link"]');
    let pointer = 'mouse';
    r.addEventListener('pointerdown', (e) => { pointer = e.pointerType; });
    // A click on the row outside its links and buttons toggles it; a double click opens it
    // (its second click doesn't toggle back). A tap opens it, unless in select mode.
    r.addEventListener('click', (e) => {
      if (held) { held = false; e.preventDefault(); return; }
      if (e.target.closest('a, button, input, label')) return;
      if (e.detail > 1) return;
      if (pointer === 'touch' && !selecting() && link) { link.click(); return; }
      toggle(r, e.shiftKey);
    });
    r.addEventListener('dblclick', (e) => {
      if (!link || pointer === 'touch' || e.target.closest('button, input, label')) return;
      e.preventDefault();
      location.href = link.href;
    });
    // A name (file or folder) selects its row like a click on the row; a double click opens
    // it. Taps still open (phones select from the row menu or by holding), except in select
    // mode, where they toggle; Enter and the keyboard maps open too (their clicks have no
    // pointer); Ctrl/⌘/Alt clicks and the middle button stay the browser's. Firefox gives
    // the click that Enter fires on a link `detail` 1 like a mouse click, so the link
    // remembers the key. The click that ends a hold is the row's (above).
    if (link) {
      let viaKey = false;
      link.addEventListener('keydown', (e) => { viaKey = e.key === 'Enter'; });
      link.addEventListener('pointerdown', () => { viaKey = false; });
      link.addEventListener('click', (e) => {
        const key = viaKey;
        viaKey = false;
        if (held || key || e.detail === 0 || e.button !== 0 || e.ctrlKey || e.metaKey || e.altKey) return;
        if (pointer === 'touch') {
          if (!selecting()) return;
          e.preventDefault();
          toggle(r);
          return;
        }
        e.preventDefault();
        if (e.detail === 1) toggle(r, e.shiftKey);
      });
    }
  }

  if (all) {
    all.addEventListener('change', () => {
      for (const r of visible()) pick(r).checked = all.checked;
      update();
    });
  }
  const clear = $('sel-clear');
  if (clear) clear.addEventListener('click', () => selectOnly([]));
  // Esc ends select mode, also without a keyboard map (a map that used the key, say to
  // empty the filter first, has said so; window listeners run after the document's).
  addEventListener('keydown', (e) => {
    if (e.key !== 'Escape' || e.defaultPrevented || !selecting()) return;
    if (document.querySelector('dialog[open], [role="menu"]:popover-open')) return;
    const t = e.target;
    if (t instanceof Element && t.matches('input:not([type="checkbox"]), textarea, select')) return;
    selectOnly([]);
  });

  // Filter: this folder's rows only, by name, without case or accents, every word somewhere
  // in the name (the same rule as the search, fs/search.rs).
  const fold = (s) => s.normalize('NFKD').replace(/\p{M}/gu, '').toLowerCase().replace(/ß/g, 'ss');
  if (filters.length) {
    const names = new Map(rows.map((r) => [r, fold(r.dataset.name)]));
    for (const filter of filters) {
      filter.addEventListener('input', () => {
        for (const other of filters) if (other !== filter) other.value = filter.value;
        const words = fold(filter.value).split(/\s+/).filter(Boolean);
        for (const r of rows) r.hidden = !words.every((w) => names.get(r).includes(w));
        update();
      });
      filter.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && filter.value) { e.stopPropagation(); filter.value = ''; filter.dispatchEvent(new Event('input')); }
      });
    }
  }

  function submit(what) {
    op.value = what;
    form.submit();
  }

  // Selection: one zip.
  const selZip = document.querySelector('#dl-sel [data-js="dl-zip"]');
  if (selZip) selZip.addEventListener('click', () => { if (selected().length) submit('zip'); });

  // Delete to trash after the confirm dialog.
  const delDialog = $('delete-dialog');
  function askDelete(list) {
    if (!delDialog || !list.length) return;
    selectOnly(list);
    const title = $('delete-title', delDialog);
    title.textContent = list.length === 1
      ? fill(ds.tDeleteName, { name: list[0].dataset.name })
      : fill(ds.tDeleteOther, { count: String(list.length) });
    delDialog.returnValue = '';
    delDialog.showModal();
  }
  if (delDialog) {
    delDialog.addEventListener('close', () => {
      if (delDialog.returnValue === 'delete') submit('delete');
    });
  }
  const del = $('delete');
  if (del) del.addEventListener('click', () => askDelete(selected()));

  const share = $('share');
  if (share) {
    share.addEventListener('click', () => {
      const sel = selected();
      if (sel.length === 1 && sel[0].dataset.share) location.href = sel[0].dataset.share;
    });
  }

  // New folder.
  const nfDialog = $('new-folder-dialog');
  const nf = $('new-folder');
  if (nf && nfDialog) {
    const nfForm = nfDialog.querySelector('form');
    const input = nfDialog.querySelector('input[name="name"]');
    const field = $('new-folder-field', nfDialog);
    const error = $('new-folder-error', nfDialog);
    const clearError = () => { delete field.dataset.state; error.textContent = ''; };
    nf.addEventListener('click', () => {
      clearError();
      nfDialog.showModal();
      input.value = '';
      input.focus();
    });
    input.addEventListener('input', clearError);
    // Sent from here, so a taken or invalid name shows in the dialog instead of a page.
    nfForm.addEventListener('submit', async (e) => {
      e.preventDefault();
      let res = null;
      try {
        res = await fetch(nfForm.action, {
          method: 'POST',
          headers: { accept: 'application/json' },
          body: new URLSearchParams(new FormData(nfForm)),
        });
      } catch (err) { res = null; }
      if (res && res.ok) {
        nfDialog.close();
        location.reload();
        return;
      }
      let msg = '';
      try { msg = (await res.json()).message || ''; } catch (err) { msg = ''; }
      field.dataset.state = 'error';
      error.textContent = msg || ds.tFailed || '';
      input.focus();
      input.select();
    });
  }

  // The row menu: one menu for all rows, filled for the row it opens from.
  function menuItem(label, iconId, action) {
    const el = document.createElement(typeof action === 'string' ? 'a' : 'button');
    el.setAttribute('role', 'menuitem');
    el.className = 'menu__item';
    if (typeof action === 'string') {
      el.href = action;
    } else {
      el.type = 'button';
      el.addEventListener('click', action);
    }
    const text = document.createElement('span');
    text.className = 'menu__text';
    const lab = document.createElement('span');
    lab.className = 'menu__label';
    lab.textContent = label;
    text.append(lab);
    el.append(window.cc.icon(iconId), text);
    return el;
  }
  for (const btn of browser.querySelectorAll('[data-js="row-menu-open"]')) {
    btn.addEventListener('click', () => {
      const row = btn.closest('[data-js="row"]');
      if (!row || !rowMenu) return;
      const name = row.dataset.name;
      rowMenu.setAttribute('aria-label', btn.getAttribute('aria-label'));
      rowMenu.replaceChildren();
      // "Select" starts select mode with this row (phones have no checkboxes until then).
      if (!pick(row).checked) {
        rowMenu.append(menuItem(ds.tSelect, 'check', () => { pick(row).checked = true; last = row; update(); }));
      }
      if (row.dataset.dir) {
        rowMenu.append(menuItem(ds.tDlFolder, 'zip', `${encodeURIComponent(name)}/?zip`));
      } else {
        const a = menuItem(ds.tDlFile, 'download', row.dataset.dl);
        a.setAttribute('download', '');
        rowMenu.append(a);
      }
      if (row.dataset.share) rowMenu.append(menuItem(ds.tShare, 'share', row.dataset.share));
      // Other scripts add their items here (ops.js: rename, move, copy).
      const add = (label, iconId, action) => rowMenu.append(menuItem(label, iconId, action));
      browser.dispatchEvent(new CustomEvent('cc:row-menu', { detail: { row, add } }));
      if (delDialog) rowMenu.append(menuItem(ds.tDelete, 'trash', () => askDelete([row])));
      window.cc.openMenu(rowMenu, btn);
    });
  }

  // A page restored from the back/forward cache keeps its checkboxes.
  addEventListener('pageshow', update);
  update();

  // Folder sizes: the server adds each folder up (?du), two at a time; the item count it
  // rendered stays as the cell's tooltip.
  const duRows = rows.filter((r) => r.dataset.dir && $('du', r));
  const duHidden = ds.hidden === '1' ? '&hidden=1' : '';
  let duNext = 0;
  async function duWorker() {
    while (duNext < duRows.length) {
      const r = duRows[duNext];
      duNext += 1;
      let d = null;
      try {
        const res = await fetch(`${encodeURIComponent(r.dataset.name)}/?du${duHidden}`, { headers: { accept: 'application/json' } });
        if (res.ok) d = await res.json();
      } catch (e) { d = null; }
      if (!d || typeof d.bytes !== 'number') continue;
      const cell = $('du', r);
      cell.title = cell.textContent.trim();
      cell.textContent = `${d.complete ? '' : '≥ '}${sizeText(d.bytes)}`;
      r.dataset.size = String(d.bytes);
      r.dataset.du = '1';
      if (pick(r).checked) update();
    }
  }
  // A share's "Download all": the folder's whole size, when the page came without it (a
  // big folder). Asked after the rows, whose walks it repeats; left out while unknown.
  async function duAll() {
    const el = document.querySelector('[data-js="dl-all-size"]');
    if (!el || el.textContent.trim()) return;
    let d = null;
    try {
      const res = await fetch(`${location.pathname}?du`, { headers: { accept: 'application/json' } });
      if (res.ok) d = await res.json();
    } catch (e) { d = null; }
    if (d && d.complete && typeof d.bytes === 'number') el.textContent = fill(el.dataset.t, { size: sizeText(d.bytes) });
  }
  Promise.all(duRows.length ? [duWorker(), duWorker()] : []).then(duAll);
  window.cc = Object.assign(window.cc || {}, {
    browser: { rows, selected, selectOnly, update, sizeText, items, setState, toggle, askDelete, submit, pick },
  });
})();
