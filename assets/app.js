// chestcraft page basics, on every page: menus (popovers) placed next to the button that
// opened them, arrow keys inside menus, the password reveal button, dialog cancel buttons,
// the type icon for thumbnails that fail to load, the sidebar drawer on narrow screens and
// a message shown after a reload.
// Hooks are data-js attributes and ARIA roles; JS never relies on classes (ui-handoff §5).
(() => {
  'use strict';
  const GAP = 6;

  // The element a menu belongs to: set by scripts that open a menu themselves
  // (`menu.ccAnchor = button`), else the button that names it in popovertarget.
  function anchorOf(menu) {
    if (menu.ccAnchor && menu.ccAnchor.isConnected) return menu.ccAnchor;
    return document.querySelector(`[popovertarget="${CSS.escape(menu.id)}"]`);
  }

  // Menus open in the top layer (no panel clips them); put each below or above its
  // button, whichever half of the window the button is in, aligned to the nearer edge.
  function place(menu) {
    const btn = anchorOf(menu);
    if (!btn) return;
    const r = btn.getBoundingClientRect();
    const s = menu.style;
    s.left = s.right = s.top = s.bottom = 'auto';
    s.maxHeight = '';
    if (r.left + r.width / 2 > innerWidth / 2) s.right = `${Math.max(GAP, innerWidth - r.right)}px`;
    else s.left = `${Math.max(GAP, r.left)}px`;
    let room;
    if (r.top + r.height / 2 > innerHeight / 2) {
      s.bottom = `${innerHeight - r.top + GAP}px`;
      room = r.top - 2 * GAP;
    } else {
      s.top = `${r.bottom + GAP}px`;
      room = innerHeight - r.bottom - 2 * GAP;
    }
    s.maxHeight = `${Math.max(120, room)}px`;
    s.overflowY = 'auto';
  }

  const items = (menu) =>
    [...menu.querySelectorAll('[role="menuitem"]')].filter((el) => !el.hidden && !el.disabled);

  document.addEventListener('beforetoggle', (e) => {
    const menu = e.target;
    if (menu.getAttribute('role') === 'menu' && e.newState === 'open') place(menu);
  }, true);

  document.addEventListener('toggle', (e) => {
    const menu = e.target;
    if (menu.getAttribute('role') !== 'menu') return;
    const btn = anchorOf(menu);
    if (btn) btn.setAttribute('aria-expanded', String(e.newState === 'open'));
    if (e.newState === 'open') {
      const first = items(menu)[0];
      if (first) first.focus({ preventScroll: true });
    }
  }, true);

  document.addEventListener('keydown', (e) => {
    const menu = e.target.closest && e.target.closest('[role="menu"]');
    if (!menu) return;
    const list = items(menu);
    const i = list.indexOf(e.target);
    let next = null;
    if (e.key === 'ArrowDown') next = list[(i + 1) % list.length];
    else if (e.key === 'ArrowUp') next = list[(i - 1 + list.length) % list.length];
    else if (e.key === 'Home') next = list[0];
    else if (e.key === 'End') next = list[list.length - 1];
    else if (e.key === 'Tab') { menu.hidePopover(); return; }
    if (next) { e.preventDefault(); next.focus(); }
  });

  // An open menu would drift away from its button.
  const closeMenus = () => {
    for (const m of document.querySelectorAll('[role="menu"]:popover-open')) m.hidePopover();
  };
  addEventListener('resize', closeMenus);
  addEventListener('scroll', closeMenus, { passive: true });
  // A menu item that runs something closes its menu.
  document.addEventListener('click', (e) => {
    const item = e.target.closest && e.target.closest('[role="menuitem"]');
    const menu = item && item.closest('[role="menu"]');
    if (menu && menu.matches(':popover-open')) menu.hidePopover();
  });

  // Password fields: show or hide what was typed (rendered hidden for pages without JS).
  for (const btn of document.querySelectorAll('[data-js="pw-reveal"]')) {
    const input = btn.parentElement.querySelector('input');
    if (!input) continue;
    btn.hidden = false;
    btn.addEventListener('click', () => {
      const show = input.type === 'password';
      input.type = show ? 'text' : 'password';
      btn.setAttribute('aria-pressed', String(show));
      input.focus();
    });
  }

  document.addEventListener('click', (e) => {
    const btn = e.target.closest && e.target.closest('[data-js="dialog-cancel"]');
    if (btn) btn.closest('dialog').close('cancel');
  });

  // A thumbnail that fails to load (no preview possible: 404; too many new renders for an
  // anonymous visitor: 429) gives way to the fallback the server put in its box: the type
  // icon in listings, the "no preview" stage in the viewer. The picture is asked for again
  // a few times, so thumbnails of a busy server still arrive; a failure the server cached
  // answers fast and stays.
  const RETRY_S = [5, 15, 45];
  function thumbFailed(img) {
    const box = img.closest('[data-js~="thumb-box"]');
    const tpl = box && box.querySelector(':scope > template[data-js="thumb-fallback"]');
    if (!tpl || box.ccKept) return;
    box.ccKept = { nodes: [...box.childNodes], state: box.dataset.state };
    box.replaceChildren(tpl.content.cloneNode(true), tpl);
    if (tpl.dataset.state) box.dataset.state = tpl.dataset.state;
    // A picture that failed again after coming back stays an icon.
    if (!img.ccBack) retryThumb(box, img, img.currentSrc || img.src, 0);
  }
  function retryThumb(box, img, src, n) {
    if (n >= RETRY_S.length) return;
    const wait = RETRY_S[n] * 1000 * (1 + Math.random() / 2);
    setTimeout(() => {
      const probe = new Image();
      probe.addEventListener('load', () => {
        const kept = box.ccKept;
        box.ccKept = null;
        box.replaceChildren(...kept.nodes);
        if (kept.state === undefined) delete box.dataset.state;
        else box.dataset.state = kept.state;
        img.ccBack = true;
        img.src = src;
      });
      probe.addEventListener('error', () => retryThumb(box, img, src, n + 1));
      probe.src = src;
    }, wait);
  }
  document.addEventListener(
    'error',
    (e) => {
      if (e.target instanceof HTMLImageElement && e.target.matches('[data-js~="thumb"]')) {
        thumbFailed(e.target);
      }
    },
    true,
  );
  // Pictures that failed before this script ran.
  for (const img of document.querySelectorAll('img[data-js~="thumb"]')) {
    if (img.complete && img.naturalWidth === 0) thumbFailed(img);
  }

  // Below 1024 px the sidebar is a drawer: the app bar's menu button opens it; Escape, a
  // click outside it or the button again close it.
  const navBtn = document.querySelector('[data-js="nav-toggle"]');
  const nav = navBtn && document.getElementById(navBtn.getAttribute('aria-controls'));
  if (nav) {
    const setNav = (open) => {
      if (open) nav.dataset.state = 'open';
      else delete nav.dataset.state;
      navBtn.setAttribute('aria-expanded', String(open));
    };
    navBtn.addEventListener('click', () => {
      const open = nav.dataset.state !== 'open';
      setNav(open);
      if (open) {
        const here = nav.querySelector('[aria-current="page"]') || nav.querySelector('a');
        if (here) here.focus({ preventScroll: true });
      }
    });
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && nav.dataset.state === 'open') { setNav(false); navBtn.focus(); }
    });
    document.addEventListener('click', (e) => {
      if (nav.dataset.state === 'open' && !nav.contains(e.target) && !navBtn.contains(e.target)) setNav(false);
    });
    addEventListener('resize', () => { if (innerWidth >= 1024 && nav.dataset.state === 'open') setNav(false); });
  }

  // A message for after a reload (an upload or a paste that changed the listing), shown in
  // the queue toast for a few seconds.
  const FLASH = 'cc-flash';
  try {
    const text = sessionStorage.getItem(FLASH);
    const queue = document.querySelector('[data-js="queue"]');
    if (text) {
      sessionStorage.removeItem(FLASH);
      if (queue) {
        queue.querySelector('[data-js="queue-text"]').textContent = text;
        const fill = queue.querySelector('[data-js="queue-fill"]');
        if (fill) fill.setAttribute('width', '100');
        queue.hidden = false;
        setTimeout(() => { if (queue.querySelector('[data-js="queue-text"]').textContent === text) queue.hidden = true; }, 6000);
      }
    }
  } catch (e) { /* storage off: no message */ }

  window.cc = Object.assign(window.cc || {}, {
    // Reload the page and show `text` after it.
    reloadWith(text) {
      try { if (text) sessionStorage.setItem(FLASH, text); } catch (e) { /* no message then */ }
      location.reload();
    },
    // Open a menu next to `anchor` (for menus a script fills, like a row's actions).
    openMenu(menu, anchor) {
      menu.ccAnchor = anchor;
      if (menu.matches(':popover-open')) menu.hidePopover();
      menu.showPopover();
    },
    // An icon from the sprite, for markup a script builds.
    icon(id) {
      const ref = document.querySelector('svg use[href*="#i-"]');
      const sprite = ref ? ref.getAttribute('href').split('#')[0] : '';
      const ns = 'http://www.w3.org/2000/svg';
      const svg = document.createElementNS(ns, 'svg');
      svg.setAttribute('class', 'icon');
      svg.setAttribute('aria-hidden', 'true');
      const use = document.createElementNS(ns, 'use');
      use.setAttribute('href', `${sprite}#i-${id}`);
      svg.append(use);
      return svg;
    },
  });
})();
