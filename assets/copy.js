// chestcraft copy buttons: <button data-js="copy" data-copy="<id of an input or textarea>">.
// Without JS (or clipboard access) the fields stay selectable by hand.
(() => {
  'use strict';
  for (const btn of document.querySelectorAll('[data-js="copy"]')) {
    btn.addEventListener('click', async () => {
      const field = document.getElementById(btn.dataset.copy);
      if (!field) return;
      try {
        await navigator.clipboard.writeText(field.value);
        btn.dataset.state = 'copied';
      } catch (e) {
        field.focus();
        field.select();
        btn.dataset.state = 'select';
      }
    });
  }
})();
