// chestcraft share dialog (review Q19), over the folder browser and the viewer. Markup:
// share_views::share_dialog (src/web/share_views.rs); copy from its data-t-* attributes.
// Without JS every way in is a link to the share page (/.cc/shares/new?vol=…&path=…), or a
// control that needs JS anyway (the selection's Share); with it, those links open this
// dialog instead:
// 1. `data-state="create"`: the target and a summary of the defaults (view, 30 days, a
//    generated passphrase); "Create link" posts the form as it is, "Change" opens the
//    options. The page's own JSON answers fill it (GET: target, allowed modes, passphrase;
//    POST: the share or field errors).
// 2. `data-state="created"`: "Copy as message" (the creator's language, or the other one),
//    link and password with copy buttons, the "shown once" warning, the details link, Done.
// The password lives only in this dialog and the clipboard: never in a URL or storage.
// A row the dialog shared gets its badge in place; focus goes back where it came from.
(() => {
  'use strict';
  const dialog = document.querySelector('[data-js="share-dialog"]');
  if (!dialog) return;
  const ds = dialog.dataset;
  const $ = (sel, root = dialog) => root.querySelector(`[data-js="${sel}"]`);
  const fill = (tpl, args) => tpl.replace(/\{(\w+)\}/g, (m, k) => (k in args ? args[k] : m));
  const form = $('share-form');
  const options = $('share-options');
  const change = $('share-change');
  const createBtn = $('share-create');
  const summary = $('share-summary');
  const formError = $('share-error-form');
  const phrase = $('share-passphrase');
  const phraseInput = $('share-passphrase-input');
  const message = $('share-message');
  const copyStatus = $('share-copy-status');
  const FIELDS = ['own_password', 'note', 'max_mib'];

  let href = ''; // the page URL this dialog stands in for
  let trigger = null; // where the focus goes back to
  let result = null; // the created share (step 2)
  let busy = false;

  const radios = (name) => [...form.querySelectorAll(`input[name="${name}"]`)];
  const checked = (name) => radios(name).find((r) => r.checked && !r.closest('label').hidden);
  const labelText = (input) => input.closest('label').textContent.trim();
  const copyText = (text, field) => (window.cc && window.cc.copyText
    ? window.cc.copyText(text, field)
    : navigator.clipboard.writeText(text).then(() => 'copied', () => 'select'));

  // The summary line and the fields that depend on the choices: the size limit only for
  // upload modes, the own password only when picked.
  function sync() {
    const mode = checked('mode');
    const expiry = checked('expiry');
    const own = (checked('password_kind') || {}).value === 'own';
    $('share-field-own_password').hidden = !own;
    $('share-field-max_mib').hidden = !mode || mode.value === 'view';
    summary.textContent = [mode && labelText(mode), expiry && labelText(expiry), own ? ds.tOwn : ds.tGenerated]
      .filter(Boolean).join(' · ');
  }

  function expand(open) {
    options.hidden = !open;
    change.setAttribute('aria-expanded', String(open));
  }

  function setStep(step) {
    dialog.dataset.state = step;
    dialog.setAttribute('aria-labelledby', step === 'created' ? 'share-dialog-done-title' : 'share-dialog-title');
  }

  function clearError(field) {
    const block = $(`share-field-${field}`);
    if (block) delete block.dataset.state;
    const text = $(`share-error-${field}`);
    if (text) text.textContent = '';
  }
  function clearErrors() {
    FIELDS.forEach(clearError);
    formError.hidden = true;
    formError.textContent = '';
  }
  // Field errors go to their field (the options open for them), the rest above the form.
  function showErrors(errors) {
    let focus = null;
    for (const [field, msg] of Object.entries(errors)) {
      const block = FIELDS.includes(field) && $(`share-field-${field}`);
      if (block) {
        expand(true);
        block.hidden = false;
        block.dataset.state = 'error';
        $(`share-error-${field}`).textContent = msg;
        focus = focus || block.querySelector('input, textarea');
      } else {
        formError.textContent = msg || ds.tFailed;
        formError.hidden = false;
      }
    }
    if (focus) { focus.focus(); if (focus.select) focus.select(); } else createBtn.focus();
  }

  function setPhrase(p) {
    phrase.textContent = p || '';
    phraseInput.value = p || '';
  }

  async function info(url) {
    try {
      const res = await fetch(url, { headers: { accept: 'application/json' }, credentials: 'same-origin' });
      if (res.ok) return await res.json();
    } catch (e) { /* the page says what went wrong */ }
    return null;
  }

  // Open the dialog for the page URL `url` (`/.cc/shares/new?vol=…&path=…`); `from` gets
  // the focus back. Anything unexpected opens the page itself, which explains.
  async function open(url, from) {
    if (busy || dialog.open) return;
    busy = true;
    const data = await info(url);
    busy = false;
    if (!data || !Array.isArray(data.modes) || !data.modes.length) { location.href = url; return; }
    const q = new URL(url, location.href).searchParams;
    href = url;
    trigger = from || null;
    result = null;
    form.reset();
    clearErrors();
    expand(false);
    setStep('create');
    $('share-vol').value = q.get('vol') || '';
    $('share-path').value = q.get('path') || '';
    setPhrase(data.passphrase);
    for (const r of radios('mode')) r.closest('label').hidden = !data.modes.includes(r.value);
    if (!checked('mode')) {
      const first = radios('mode').find((r) => !r.closest('label').hidden);
      if (first) first.checked = true;
    }
    $('share-target').textContent = `${fill(ds.tTarget, { name: data.name })} ${data.location}`;
    sync();
    dialog.showModal();
    createBtn.focus();
  }

  form.addEventListener('change', sync);
  for (const field of FIELDS) {
    const input = $(`share-field-${field}`).querySelector('input, textarea');
    input.addEventListener('input', () => clearError(field));
  }
  for (const r of radios('password_kind')) r.addEventListener('change', () => clearError('own_password'));

  change.addEventListener('click', () => {
    const open = options.hidden;
    expand(open);
    if (open) {
      const first = checked('mode');
      if (first) first.focus();
    }
  });

  $('share-regenerate').addEventListener('click', async () => {
    const data = await info(href);
    if (data && data.passphrase) setPhrase(data.passphrase);
    const generated = radios('password_kind').find((r) => r.value === 'generated');
    if (generated) generated.checked = true;
    sync();
  });

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (busy) return;
    busy = true;
    form.setAttribute('aria-busy', 'true');
    clearErrors();
    let res = null;
    let body = null;
    try {
      res = await fetch(form.action, {
        method: 'POST',
        headers: { accept: 'application/json' },
        body: new URLSearchParams(new FormData(form)),
        credentials: 'same-origin',
      });
      body = await res.json();
    } catch (err) { body = null; }
    busy = false;
    form.removeAttribute('aria-busy');
    if (res && res.ok && body && body.link) created(body);
    else showErrors((body && body.errors) || { form: ds.tFailed });
  });

  // Step 2.
  function setLang(code) {
    for (const b of dialog.querySelectorAll('[data-js="share-lang"]')) {
      const on = b.value === code;
      b.setAttribute('aria-pressed', String(on));
      if (on) b.setAttribute('aria-current', 'true'); else b.removeAttribute('aria-current');
    }
    message.value = (result.message && result.message[code]) || '';
  }

  function say(text) {
    copyStatus.textContent = '';
    setTimeout(() => { copyStatus.textContent = text; }, 30);
  }

  function created(body) {
    result = body;
    $('share-link').value = body.link;
    $('share-password').value = body.password;
    $('share-until').textContent = body.expires;
    $('share-details').href = body.details_href;
    message.hidden = true;
    copyStatus.textContent = '';
    setLang(body.lang === 'de' ? 'de' : 'en');
    setStep('created');
    // Closed (Esc) while the request ran: the password shows only here, so show it.
    if (!dialog.open) dialog.showModal();
    $('share-copy-message').focus();
    badge(body.badge);
  }

  // The shared row in the listing gets its badge (the listing has no other way to know).
  function badge(title) {
    const tpl = document.querySelector('template[data-js="share-badge"]');
    if (!tpl) return;
    const row = [...document.querySelectorAll('[data-js="row"]')].find((r) => r.dataset.share === href);
    const link = row && row.querySelector('[data-js="row-link"]');
    if (!link) return;
    let b = row.querySelector('[data-js="sharebadge"]');
    if (!b) {
      b = tpl.content.firstElementChild.cloneNode(true);
      b.dataset.js = 'sharebadge';
      link.after(' ', b);
    }
    b.title = title;
  }

  for (const b of dialog.querySelectorAll('[data-js="share-lang"]')) {
    b.addEventListener('click', () => { if (result) setLang(b.value); });
  }
  $('share-copy-message').addEventListener('click', async () => {
    if (!result) return;
    const how = await copyText(message.value, message);
    say(how === 'copied' ? ds.tCopiedMessage : ds.tCopyByHand);
  });
  for (const b of dialog.querySelectorAll('[data-js="share-copy"]')) {
    b.addEventListener('click', async () => {
      const field = document.getElementById(b.dataset.copy);
      if (!field || !field.value) return;
      const how = await copyText(field.value, field);
      const done = b.dataset.what === 'password' ? ds.tCopiedPassword : ds.tCopiedLink;
      say(how === 'copied' ? done : ds.tCopyByHand);
    });
  }
  $('share-close').addEventListener('click', () => dialog.close());

  dialog.addEventListener('close', () => {
    // The password goes with the dialog.
    $('share-password').value = '';
    message.value = '';
    result = null;
    if (trigger && trigger.isConnected) trigger.focus();
    trigger = null;
  });

  // Links to the share page (the viewer's Share, the row menu's) open the dialog; other
  // clicks (new tab, modifiers) keep the page.
  document.addEventListener('click', (e) => {
    const a = e.target.closest && e.target.closest('a[data-js~="share-open"]');
    if (!a || e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    const menu = a.closest('[role="menu"]');
    open(a.getAttribute('href'), (menu && menu.ccAnchor) || a);
  });

  window.cc = Object.assign(window.cc || {}, { share: { open } });
})();
