// chestcraft account page and background editor. Both work without script; with it:
// - Account page (`form[data-js="prefs"]`): a changed select saves at once in the
//   background and says so in the form's status line; a new language reloads the page.
//   The save button is only for pages without script.
// - Background editor (`form[data-js="bg-editor"]`): the preview follows every choice
//   (tile, own colors, glass, veil, blur, opacity) before it is saved; a background tile and
//   the picture sets exclude each other (several sets may be ticked: they take turns), the
//   rotation shows from two ticked sets, the own color's fields open with its tile, and the
//   upload tile opens its form in a dialog.
// Hooks are data-js attributes; JS never relies on classes (ui-handoff §5).
(() => {
  'use strict';
  const js = (name, root = document) => root.querySelector(`[data-js~="${name}"]`);
  const all = (name, root = document) => [...root.querySelectorAll(`[data-js~="${name}"]`)];

  // ---------------------------------------------------------------- account page
  const prefs = js('prefs');
  if (prefs) {
    const status = js('prefs-status', prefs);
    const submit = js('prefs-submit', prefs);
    if (submit) submit.hidden = true;
    let pending = 0;
    prefs.addEventListener('change', async (e) => {
      const field = e.target;
      if (!(field instanceof HTMLSelectElement)) return;
      const mine = ++pending;
      status.textContent = '';
      let ok = false;
      try {
        const res = await fetch(prefs.action, {
          method: 'POST',
          body: new URLSearchParams(new FormData(prefs)),
          credentials: 'same-origin',
          redirect: 'manual',
        });
        ok = res.type === 'opaqueredirect' || res.ok;
      } catch (err) { /* offline: the plain post below says so */ }
      if (!ok) {
        // Let the server answer in full (an expired session, a refused value).
        HTMLFormElement.prototype.submit.call(prefs);
        return;
      }
      if (field.name === 'lang') { location.reload(); return; }
      if (mine === pending) status.textContent = status.dataset.tSaved;
    });
  }

  // ---------------------------------------------------------------- background editor
  const editor = js('bg-editor');
  if (!editor) return;
  const preview = js('bg-preview');
  const shown = preview && preview.firstElementChild; // the preview's .backdrop
  const picks = all('bg-pick', editor);
  const radios = picks.filter((i) => i.type === 'radio');
  const sets = picks.filter((i) => i.type === 'checkbox');
  const imagesRadio = radios.find((i) => i.value === 'images');
  const colorRadio = radios.find((i) => i.value === 'color');
  const rotation = js('bg-rotation', editor);
  const colorFields = js('bg-colors', editor);
  const swatchOf = (input) => js('bg-swatch', input.closest('label')).firstElementChild;

  // The images radio is for pages without script: ticking a set says it.
  const imagesRow = js('bg-images', editor);
  if (imagesRow) imagesRow.hidden = true;

  const field = (name) => editor.elements.namedItem(name);
  const value = (name) => {
    const f = field(name);
    return f ? f.value : '';
  };

  // The own color as CSS, as /.cc/backdrop.css writes it.
  function ownColor() {
    const n = Number(value('stops')) || 1;
    const colors = [1, 2, 3].slice(0, n).map((i) => value(`c${i}`));
    if (n === 1) return colors[0];
    const angle = (Number(value('angle')) || 0) % 360;
    return `linear-gradient(${angle}deg, ${colors.join(', ')})`;
  }

  // Fields beyond the chosen number of colors are not used.
  function showStops() {
    const n = Number(value('stops')) || 1;
    all('bg-color', editor).forEach((f, i) => { f.hidden = i >= n; });
  }

  // The tile whose background the preview shows: the checked radio's, or the first
  // ticked set's.
  function source() {
    const radio = radios.find((r) => r.checked);
    if (radio && radio !== imagesRadio) return radio;
    return sets.find((s) => s.checked) || null;
  }

  function draw() {
    if (!shown) return;
    const glass = (editor.querySelector('[data-js="bg-glass"]:checked') || {}).value || 'frosted';
    preview.dataset.glass = glass;
    const opacity = value('opacity');
    if (opacity) preview.dataset.opacity = opacity;
    else delete preview.dataset.opacity;
    const src = source();
    if (src) {
      const from = swatchOf(src);
      shown.dataset.state = from.dataset.state;
      shown.replaceChildren(...[...from.children].map((c) => c.cloneNode(true)));
      if (from.dataset.state === 'color') {
        const color = src === colorRadio ? ownColor()
          : getComputedStyle(from).getPropertyValue('--backdrop-color').trim();
        shown.style.setProperty('--backdrop-color', color);
      } else {
        shown.style.removeProperty('--backdrop-color');
      }
    }
    // Automatic veil: none over the skin's plain color, else light (frosted) or medium.
    const dim = value('dim');
    shown.dataset.dim = dim !== '' ? dim : shown.dataset.state === 'none' ? '0' : glass === 'clear' ? '2' : '1';
    shown.dataset.blur = value('blur') || '0';
    if (rotation) rotation.hidden = sets.filter((s) => s.checked).length < 2;
  }

  function changed(t) {
    if (t.type === 'checkbox' && sets.includes(t)) {
      // A set: the background is the ticked sets now (or, with none left, the first tile).
      if (sets.some((s) => s.checked)) imagesRadio.checked = true;
      else if (imagesRadio.checked) radios[0].checked = true;
    } else if (t.type === 'radio' && t.name === 'fill' && t !== imagesRadio) {
      for (const s of sets) s.checked = false;
      if (colorFields) colorFields.open = t === colorRadio;
    } else if (colorFields && colorFields.contains(t)) {
      // Changing a color picks the own color.
      colorRadio.checked = true;
      for (const s of sets) s.checked = false;
      showStops();
      swatchOf(colorRadio).style.setProperty('--backdrop-color', ownColor());
    }
    draw();
  }
  editor.addEventListener('change', (e) => changed(e.target));
  // Colors change while the picker is open.
  editor.addEventListener('input', (e) => {
    if (colorFields && colorFields.contains(e.target)) changed(e.target);
  });
  showStops();

  // The upload tile: its form in a dialog.
  const upload = js('bg-upload');
  const dialog = js('bg-upload-dialog');
  if (upload && dialog) {
    const body = js('bg-upload-body', upload);
    const summary = upload.querySelector(':scope > summary');
    const open = () => {
      if (body.parentElement !== dialog) dialog.append(body);
      for (const b of all('dialog-cancel', body)) b.hidden = false;
      dialog.showModal();
      const first = body.querySelector('input');
      if (first) first.focus();
    };
    summary.addEventListener('click', (e) => { e.preventDefault(); open(); });
    // The server sent the form back with an error: open it as a dialog at once.
    if (upload.open) { upload.open = false; open(); }
  }
})();
