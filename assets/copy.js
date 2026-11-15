// chestcraft copy buttons: <button data-js="copy" data-copy="<id of an input or textarea>">,
// rendered hidden (they need script) and shown here; `cc.copyButtons(root)` shows the ones in
// markup a script inserts (forms.js puts "shown once" boxes into dialogs), and
// `cc.copyText(text, field)` copies for scripts (the share dialog). The clipboard needs a
// secure context (HTTPS or localhost); elsewhere, or without clipboard access, the field is
// selected so it can be copied by hand. Without JS the fields stay selectable as they are.
(() => {
  'use strict';
  // Resolves to 'copied', or 'select' when the field had to be selected instead.
  async function copyText(text, field) {
    try {
      await navigator.clipboard.writeText(text);
      return 'copied';
    } catch (e) {
      if (field) {
        field.hidden = false;
        field.focus();
        field.select();
      }
      return 'select';
    }
  }
  function copyButtons(root) {
    for (const btn of root.querySelectorAll('[data-js="copy"][data-copy]')) btn.hidden = false;
  }
  document.addEventListener('click', async (e) => {
    const btn = e.target.closest && e.target.closest('[data-js="copy"][data-copy]');
    if (!btn) return;
    const field = document.getElementById(btn.dataset.copy);
    if (field) btn.dataset.state = await copyText(field.value, field);
  });
  copyButtons(document);
  window.cc = Object.assign(window.cc || {}, { copyText, copyButtons });
})();
