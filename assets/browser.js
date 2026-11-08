// chestcraft folder browser: selection and the status bar, the filter, the row menu, new
// folder, delete to trash and the selection's zip. Markup: views::listing (src/web/views.rs);
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
  const filter = $('filter');
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
  const visible = () => rows.filter((r) => !r.hidden);

  // What the selection's actions and its download menu show.
  function update() {
    const sel = selected();
    for (const r of rows) setState(r, 'selected', pick(r).checked);
    if (status) status.dataset.state = sel.length ? 'select' : 'idle';
    const bytes = sel.reduce((n, r) => n + (r.dataset.dir ? 0 : Number(r.dataset.size) || 0), 0);
    const files = sel.some((r) => !r.dataset.dir);
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
    // Phones: hold a row to select it (the checkboxes show once something is selected).
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
    // A click on the row outside its links and buttons toggles it.
    r.addEventListener('click', (e) => {
      if (held) { held = false; e.preventDefault(); return; }
      if (e.target.closest('a, button, input, label')) return;
      toggle(r, e.shiftKey);
    });
  }

  if (all) {
    all.addEventListener('change', () => {
      for (const r of visible()) pick(r).checked = all.checked;
      update();
    });
  }
  const clear = $('sel-clear');
  if (clear) clear.addEventListener('click', () => selectOnly([]));

  // Filter: this folder's rows only, by name, without case or accents.
  const fold = (s) => s.normalize('NFKD').replace(/\p{M}/gu, '').toLowerCase();
  if (filter) {
    const names = new Map(rows.map((r) => [r, fold(r.dataset.name)]));
    filter.addEventListener('input', () => {
      const q = fold(filter.value.trim());
      for (const r of rows) r.hidden = q !== '' && !names.get(r).includes(q);
      update();
    });
    filter.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && filter.value) { e.stopPropagation(); filter.value = ''; filter.dispatchEvent(new Event('input')); }
    });
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
    nf.addEventListener('click', () => {
      nfDialog.showModal();
      const input = nfDialog.querySelector('input[name="name"]');
      input.value = '';
      input.focus();
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
      if (row.dataset.dir) {
        rowMenu.append(menuItem(ds.tDlFolder, 'zip', `${encodeURIComponent(name)}/?zip`));
      } else {
        const a = menuItem(ds.tDlFile, 'download', row.dataset.dl);
        a.setAttribute('download', '');
        rowMenu.append(a);
      }
      if (row.dataset.share) rowMenu.append(menuItem(ds.tShare, 'share', row.dataset.share));
      if (delDialog) rowMenu.append(menuItem(ds.tDelete, 'trash', () => askDelete([row])));
      rowMenu.dispatchEvent(new CustomEvent('cc:row-menu', { detail: { row } }));
      window.cc.openMenu(rowMenu, btn);
    });
  }

  // A page restored from the back/forward cache keeps its checkboxes.
  addEventListener('pageshow', update);
  update();
  window.cc = Object.assign(window.cc || {}, { browser: { rows, selected, selectOnly, update, sizeText, items } });
})();
