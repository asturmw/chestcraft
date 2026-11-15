// chestcraft copy buttons: <button data-js="copy" data-copy="<id of an input or textarea>">,
// and `cc.copyText(text, field)` for scripts (the share dialog). The clipboard needs a
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
  for (const btn of document.querySelectorAll('[data-js="copy"][data-copy]')) {
    btn.addEventListener('click', async () => {
      const field = document.getElementById(btn.dataset.copy);
      if (field) btn.dataset.state = await copyText(field.value, field);
    });
  }
  window.cc = Object.assign(window.cc || {}, { copyText });
})();
