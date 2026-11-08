// chestcraft keyboard maps (ui-handoff §6 "Keyboard"): `data-keys` on <html> is vim,
// standard or off. Keys never fire while focus is in a text field, a dialog or a menu.
// The cursor row is data-state="cursor" (distinct from the selection) and holds the focus
// on its link, so Enter opens it and screen readers follow.
(() => {
  'use strict';
  const mode = document.documentElement.dataset.keys;
  if (mode !== 'vim' && mode !== 'standard') return;
  const vim = mode === 'vim';
  const js = (sel) => document.querySelector(`[data-js="${sel}"]`);
  const help = js('keys-help');
  const B = window.cc && window.cc.browser;
  const ops = () => window.cc && window.cc.ops;

  function busy(e) {
    const t = e.target;
    if (document.querySelector('dialog[open]')) return true;
    if (!(t instanceof Element)) return false;
    if (t.closest('[role="menu"]')) return true;
    return t.matches('input:not([type="checkbox"]):not([type="radio"]), textarea, select, [contenteditable]:not([contenteditable="false"])');
  }
  const click = (sel) => { const el = js(sel); if (el && !el.hidden && !el.disabled) { el.click(); return true; } return false; };
  const showHelp = () => { if (help) help.showModal(); };

  // --- the viewer: previous, next, close ----------------------------------------------
  if (!B) {
    document.addEventListener('keydown', (e) => {
      if (busy(e) || e.ctrlKey || e.metaKey || e.altKey) return;
      const k = e.key;
      let done = false;
      if (k === '?') { showHelp(); done = true; }
      else if (vim && k === 'h') done = click('viewer-prev');
      else if (vim && k === 'l') done = click('viewer-next');
      else if (vim && k === 'q') done = click('viewer-close');
      else if (!vim && k === 'ArrowLeft') done = click('viewer-prev');
      else if (!vim && k === 'ArrowRight') done = click('viewer-next');
      else if (!vim && k === 'Escape') done = click('viewer-close');
      if (done) e.preventDefault();
    });
    return;
  }

  // --- folder pages ---------------------------------------------------------------------------
  const filter = js('filter');
  const visible = () => B.rows.filter((r) => !r.hidden);
  let cursor = null;
  let anchor = null; // range start (vim V, standard Shift+arrows)
  let visual = false;

  function setCursor(row) {
    if (!row) return;
    if (cursor && cursor !== row) B.setState(cursor, 'cursor', false);
    cursor = row;
    B.setState(row, 'cursor', true);
    const link = row.querySelector('a');
    if (link) link.focus({ preventScroll: true });
    row.scrollIntoView({ block: 'nearest' });
  }
  function step(delta) {
    const vis = visible();
    if (!vis.length) return;
    const i = cursor ? vis.indexOf(cursor) : -1;
    const next = i < 0 ? (delta > 0 ? 0 : vis.length - 1) : Math.max(0, Math.min(vis.length - 1, i + delta));
    setCursor(vis[next]);
  }
  function edge(last) {
    const vis = visible();
    if (vis.length) setCursor(vis[last ? vis.length - 1 : 0]);
  }
  // Select from the anchor to the cursor (only these).
  function range() {
    const vis = visible();
    const [a, b] = [vis.indexOf(anchor), vis.indexOf(cursor)].sort((x, y) => x - y);
    if (a < 0) return;
    B.selectOnly(vis.slice(a, b + 1));
  }
  function toggle() {
    if (!cursor) return;
    B.pick(cursor).checked = !B.pick(cursor).checked;
    B.update();
  }
  // What an action works on: the selection, else the cursor row.
  function targets() {
    const sel = B.selected();
    if (sel.length) return sel;
    return cursor ? [cursor] : [];
  }
  function open() {
    if (!cursor) return;
    const link = cursor.querySelector('a');
    if (link) link.click();
  }
  function up() {
    location.href = new URL('../', location.href).href;
  }
  function escape() {
    if (visual) { visual = false; anchor = null; return; }
    if (filter && filter.value) {
      filter.value = '';
      filter.dispatchEvent(new Event('input'));
      return;
    }
    B.selectOnly([]);
  }
  function share() {
    const t = targets();
    if (t.length === 1 && t[0].dataset.share) location.href = t[0].dataset.share;
  }
  function zip() {
    if (B.selected().length) B.submit('zip');
    else if (cursor && cursor.dataset.dir) location.href = new URL(`${encodeURIComponent(cursor.dataset.name)}/?zip`, location.href).href;
    else {
      const form = document.querySelector('#dl-folder [data-js="dl-zip"]');
      if (form) form.click();
    }
  }
  function rename() {
    const t = targets();
    if (t.length === 1 && ops()) ops().rename(t[0]);
  }

  // The filter: Enter goes to the first match, Esc on an empty filter leaves it.
  if (filter) {
    filter.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') { e.preventDefault(); filter.blur(); edge(false); }
      else if (e.key === 'Escape' && !filter.value) filter.blur();
    });
  }

  let pending = '';
  let pendingTimer = null;
  function sequence(key) {
    clearTimeout(pendingTimer);
    const seq = pending + key;
    pending = '';
    if (seq === 'gg' || seq === 'yy' || seq === 'dd') return seq;
    if (key === 'g' || key === 'y' || key === 'd') {
      pending = key;
      pendingTimer = setTimeout(() => { pending = ''; }, 800);
    }
    return null;
  }

  function vimKey(e) {
    const k = e.key;
    const seq = sequence(k);
    if (seq === 'gg') { edge(false); return true; }
    if (seq === 'yy') { if (ops()) ops().copy(targets()); return true; }
    if (seq === 'dd') { if (ops()) ops().cut(targets()); return true; }
    if (pending) return true;
    switch (k) {
      case 'j': case 'ArrowDown': step(1); if (visual) range(); return true;
      case 'k': case 'ArrowUp': step(-1); if (visual) range(); return true;
      case 'G': edge(true); if (visual) range(); return true;
      case 'n': step(1); return true;
      case 'N': step(-1); return true;
      case 'l': open(); return true;
      case 'Enter': open(); return true;
      case 'h': case 'Backspace': up(); return true;
      case '/': if (filter) filter.focus(); return true;
      case 'Escape': escape(); return true;
      case ' ': toggle(); return true;
      case 'V':
        if (visual) { visual = false; anchor = null; return true; }
        if (!cursor) step(1);
        visual = true;
        anchor = cursor;
        range();
        return true;
      case 'p': if (ops()) ops().paste(); return true;
      case 'D': B.askDelete(targets()); return true;
      case 'r': rename(); return true;
      case 'o': return click('new-folder');
      case 's': share(); return true;
      case 'u': return click('upload-pick');
      case 'z': zip(); return true;
      case '?': showHelp(); return true;
      default: return false;
    }
  }

  function standardKey(e) {
    const k = e.key;
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl) {
      // Text the visitor selected on the page keeps the browser's own copy.
      const text = String(window.getSelection && window.getSelection()).length > 0;
      switch (k.toLowerCase()) {
        case 'a': B.selectOnly(visible()); return true;
        case 'c': if (text || !ops()) return false; ops().copy(targets()); return true;
        case 'x': if (!ops()) return false; ops().cut(targets()); return true;
        case 'v': if (!ops()) return false; ops().paste(); return true;
        default: return false;
      }
    }
    if (e.altKey) {
      if (k === 'ArrowUp') { up(); return true; }
      return false;
    }
    switch (k) {
      case 'ArrowDown':
      case 'ArrowUp':
        if (e.shiftKey) {
          if (!anchor || !B.pick(anchor).checked) anchor = cursor || visible()[0];
          step(k === 'ArrowDown' ? 1 : -1);
          range();
        } else {
          anchor = null;
          step(k === 'ArrowDown' ? 1 : -1);
        }
        return true;
      case 'Home': edge(false); return true;
      case 'End': edge(true); return true;
      case 'Enter': open(); return true;
      case ' ': toggle(); return true;
      case '/': if (filter) filter.focus(); return true;
      case 'Escape': escape(); return true;
      case 'Delete': B.askDelete(targets()); return true;
      case 'F2': rename(); return true;
      case '?': showHelp(); return true;
      default: return false;
    }
  }

  document.addEventListener('keydown', (e) => {
    if (busy(e)) return;
    // Enter and Space keep their own meaning on links, buttons and checkboxes (the cursor
    // row's link opens with the browser's own Enter).
    if (e.target instanceof Element && ((e.key === 'Enter' && e.target.closest('a, button, input, summary'))
      || (e.key === ' ' && e.target.closest('button, input, summary')))) return;
    if (vim && (e.ctrlKey || e.metaKey || e.altKey)) return;
    if (vim ? vimKey(e) : standardKey(e)) e.preventDefault();
  });
  // A click puts the cursor on its row.
  for (const r of B.rows) {
    r.addEventListener('click', (e) => { if (!e.target.closest('a')) { if (cursor && cursor !== r) B.setState(cursor, 'cursor', false); cursor = r; B.setState(r, 'cursor', true); } });
  }
})();
