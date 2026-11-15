// chestcraft admin forms (review Q26): the accounts and libraries pages, the account page's
// app passwords, the share details. Everything here is a plain form that works without
// script; with it:
// - `details[data-js="adder"]` (a create form behind its "+ …" button): the form opens in
//   a dialog and posts in the background. The answer page decides what the dialog shows:
//   a "shown once" block (`[data-js="secret-result"]`) replaces the form, "Done" (or
//   closing) loads the page again with the new row; an error (`[data-js="form-error"]`)
//   shows above the fields, which keep what was typed; anything else that redirected (a
//   create without a secret) goes where it pointed. A form the server opened again with an
//   error (after a post without script) opens as a dialog at once.
// - `form[data-confirm]` (destructive row actions): asks first in the page's confirm dialog
//   (`[data-js="confirm-dialog"]`); the post then goes as a plain form.
// Secrets live only in the dialog and the clipboard: never in a URL or storage.
(() => {
  'use strict';
  const js = (name, root = document) => root.querySelector(`[data-js="${name}"]`);
  const AFTER = 'cc-after-form';

  // After "Done": back at the section the form belongs to.
  try {
    const id = sessionStorage.getItem(AFTER);
    if (id) {
      sessionStorage.removeItem(AFTER);
      const el = document.getElementById(id);
      if (el) el.scrollIntoView({ block: 'start' });
    }
  } catch (e) { /* storage off: the page starts at the top */ }

  // The page again, by GET (the page may be the answer to a plain post: a reload would post
  // it again), scrolled to `section`.
  function reloadAt(section) {
    try { if (section && section.id) sessionStorage.setItem(AFTER, section.id); } catch (e) { /* top then */ }
    location.assign(location.pathname);
  }

  for (const adder of document.querySelectorAll('details[data-js="adder"]')) {
    const summary = adder.querySelector(':scope > summary');
    const form = adder.querySelector(':scope > form');
    if (!summary || !form) continue;
    const section = adder.closest('section[id], div[id]');
    const errorBox = js('form-error', form);
    const submit = form.querySelector('button[type="submit"]');
    const dialog = document.createElement('dialog');
    dialog.className = 'dialog adder-dialog';
    dialog.dataset.js = 'adder-dialog';
    dialog.setAttribute('aria-labelledby', form.getAttribute('aria-labelledby') || '');
    form.removeAttribute('aria-labelledby');
    dialog.append(form);
    document.body.append(dialog);
    for (const b of form.querySelectorAll('[data-js="dialog-cancel"]')) b.hidden = false;
    let busy = false;
    let done = false;

    const showError = (text) => {
      errorBox.textContent = text || adder.dataset.tFailed;
      errorBox.hidden = false;
      errorBox.scrollIntoView({ block: 'nearest' });
    };
    const open = () => {
      if (dialog.open) return;
      dialog.showModal();
      const first = form.querySelector('input:not([type="hidden"]), select, textarea');
      if (first) first.focus();
    };
    summary.addEventListener('click', (e) => { e.preventDefault(); open(); });
    dialog.addEventListener('close', () => { if (done) reloadAt(section); });

    // The answer's "shown once" block in place of the form.
    function showResult(result) {
      const block = document.importNode(result, true);
      const back = js('secret-back', block);
      if (back) back.remove();
      const h1 = block.querySelector('h1');
      if (h1) {
        const h2 = document.createElement('h2');
        h2.className = 'dialog__title';
        h2.id = `${adder.id}-done-title`;
        h2.textContent = h1.textContent;
        h1.replaceWith(h2);
        dialog.setAttribute('aria-labelledby', h2.id);
      }
      block.removeAttribute('aria-labelledby');
      const actions = document.createElement('div');
      actions.className = 'dialog__actions';
      const ok = document.createElement('button');
      ok.type = 'button';
      ok.className = 'btn btn--primary';
      ok.textContent = adder.dataset.tDone;
      ok.addEventListener('click', () => dialog.close());
      actions.append(ok);
      block.append(actions);
      form.hidden = true;
      dialog.append(block);
      if (window.cc && window.cc.copyButtons) window.cc.copyButtons(block);
      done = true;
      const copy = block.querySelector('[data-js="copy"]:not([hidden])') || ok;
      copy.focus();
    }

    form.addEventListener('submit', async (e) => {
      e.preventDefault();
      if (busy) return;
      busy = true;
      submit.disabled = true;
      errorBox.hidden = true;
      let res = null;
      let doc = null;
      try {
        res = await fetch(form.action, {
          method: 'POST',
          body: new URLSearchParams(new FormData(form)),
          credentials: 'same-origin',
        });
        doc = new DOMParser().parseFromString(await res.text(), 'text/html');
      } catch (err) { /* below: "failed" */ }
      busy = false;
      submit.disabled = false;
      if (!doc) { showError(); return; }
      const result = js('secret-result', doc);
      if (result) { showResult(result); return; }
      const error = doc.querySelector('[data-js="form-error"]:not([hidden])');
      if (error) { showError(error.textContent.trim()); return; }
      if (res.redirected && res.ok) { location.assign(res.url); return; }
      showError();
    });

    if (adder.open) {
      adder.open = false;
      open();
    }
  }

  // Destructive row actions ask first.
  const confirmDialog = js('confirm-dialog');
  if (confirmDialog) {
    const text = js('confirm-text', confirmDialog);
    const ok = js('confirm-ok', confirmDialog);
    let pending = null;
    document.addEventListener('submit', (e) => {
      const form = e.target;
      if (!(form instanceof HTMLFormElement) || !form.hasAttribute('data-confirm')) return;
      if (form.ccConfirmed) return;
      e.preventDefault();
      pending = { form, from: e.submitter || form.querySelector('button[type="submit"]') };
      text.textContent = form.dataset.confirm;
      ok.textContent = pending.from ? pending.from.textContent.trim() : '';
      confirmDialog.returnValue = '';
      confirmDialog.showModal();
      ok.focus();
    });
    confirmDialog.addEventListener('close', () => {
      const p = pending;
      pending = null;
      if (!p) return;
      if (confirmDialog.returnValue === 'ok') {
        p.form.ccConfirmed = true;
        p.form.requestSubmit(p.from);
      } else if (p.from) {
        p.from.focus();
      }
    });
  }
})();
