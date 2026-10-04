const { invoke } = window.__TAURI__.core;

const tabsEl = document.getElementById('tabs');
const tabsRow = document.getElementById('tabs-row');
const addRow = document.getElementById('add-row');
const addBtn = document.getElementById('add-btn');
const editBtn = document.getElementById('edit-btn');
const reloadBtn = document.getElementById('reload-btn');
const addName = document.getElementById('add-name');
const addUrl = document.getElementById('add-url');
const addConfirm = document.getElementById('add-confirm');
const addCancel = document.getElementById('add-cancel');

const DRAG_THRESHOLD_PX = 4;

let editing = false;
let lastState = { tabs: [], activeId: null };
let drag = null;
let suppressClick = false;

// Icons are cached on disk by the backend. Several services refuse image
// requests coming from this webview's `tauri://` origin, so fetching happens
// in Rust and arrives here as a data URI.
const iconCache = new Map();
const iconFetches = new Set();

function makeFallbackIcon(name) {
  const el = document.createElement('span');
  el.className = 'tab-icon tab-icon-fallback';
  el.textContent = (name.trim()[0] || '?').toUpperCase();
  return el;
}

function makeIcon(tab) {
  const cached = iconCache.get(tab.id);
  if (cached) {
    const img = document.createElement('img');
    img.className = 'tab-icon';
    img.src = cached;
    img.alt = '';
    return img;
  }
  loadIcon(tab);
  return makeFallbackIcon(tab.name);
}

async function loadIcon(tab) {
  if (iconFetches.has(tab.id)) return;
  iconFetches.add(tab.id);
  try {
    let uri = await invoke('tab_icon', { id: tab.id });
    if (!uri) uri = await invoke('refresh_tab_icon', { id: tab.id });
    if (uri) {
      iconCache.set(tab.id, uri);
      render(lastState);
    }
  } catch (_) {
    // Site has no reachable icon — the letter badge stands in.
  } finally {
    iconFetches.delete(tab.id);
  }
}

/// Edit mode: clicking an icon opens a picker and stores the chosen image.
function pickIconFor(tab) {
  const picker = document.createElement('input');
  picker.type = 'file';
  picker.accept = 'image/*';
  picker.style.display = 'none';
  document.body.appendChild(picker);

  picker.addEventListener('change', () => {
    const file = picker.files && picker.files[0];
    if (!file) {
      picker.remove();
      return;
    }
    const reader = new FileReader();
    reader.onload = async () => {
      try {
        const uri = await invoke('set_tab_icon', { id: tab.id, dataUri: reader.result });
        iconCache.set(tab.id, uri);
        render(lastState);
      } catch (e) {
        console.error('set_tab_icon failed', e);
      } finally {
        picker.remove();
      }
    };
    reader.readAsDataURL(file);
  });

  picker.click();
}

function render(state) {
  lastState = state;
  tabsEl.innerHTML = '';
  for (const tab of state.tabs) {
    tabsEl.appendChild(editing ? renderEditTab(tab) : renderTab(tab, state));
  }
  editBtn.textContent = editing ? '✓' : '✎';
  editBtn.title = editing ? 'Done editing' : 'Edit tabs';
  editBtn.classList.toggle('active', editing);
}

function renderTab(tab, state) {
  const el = document.createElement('div');
  el.className = 'tab' + (tab.id === state.activeId ? ' active' : '');
  el.title = tab.url;
  el.dataset.id = tab.id;

  el.appendChild(makeIcon(tab));

  const label = document.createElement('span');
  label.className = 'tab-label';
  label.textContent = tab.name;
  el.appendChild(label);

  el.addEventListener('mousedown', (ev) => {
    if (ev.button !== 0) return;
    drag = { el, startX: ev.clientX, moved: false };
  });

  el.addEventListener('click', async () => {
    if (suppressClick) {
      suppressClick = false;
      return;
    }
    render(await invoke('switch_tab', { id: tab.id }));
  });

  return el;
}

document.addEventListener('mousemove', (ev) => {
  if (!drag) return;

  if (!drag.moved) {
    if (Math.abs(ev.clientX - drag.startX) < DRAG_THRESHOLD_PX) return;
    drag.moved = true;
    drag.el.classList.add('dragging');
  }
  // Keep the pointer from selecting text / starting a native window drag.
  ev.preventDefault();

  for (const sibling of [...tabsEl.children]) {
    if (sibling === drag.el) continue;
    const r = sibling.getBoundingClientRect();
    if (ev.clientX < r.left || ev.clientX > r.right) continue;
    const dropAfter = ev.clientX > r.left + r.width / 2;
    tabsEl.insertBefore(drag.el, dropAfter ? sibling.nextSibling : sibling);
    break;
  }
});

document.addEventListener('mouseup', async () => {
  if (!drag) return;
  const finished = drag;
  drag = null;

  if (!finished.moved) return; // plain click — let the click handler switch tabs
  finished.el.classList.remove('dragging');
  suppressClick = true;

  const ids = [...tabsEl.children].map((c) => c.dataset.id);
  const unchanged = ids.every((id, i) => lastState.tabs[i] && lastState.tabs[i].id === id);
  if (unchanged) return;
  render(await invoke('reorder_tabs', { ids }));
});

function renderEditTab(tab) {
  const el = document.createElement('div');
  el.className = 'tab editing';
  el.dataset.id = tab.id;

  const iconBtn = document.createElement('button');
  iconBtn.className = 'tab-icon-btn';
  iconBtn.title = 'Change icon';
  iconBtn.appendChild(makeIcon(tab));
  iconBtn.addEventListener('mousedown', (ev) => ev.preventDefault());
  iconBtn.addEventListener('click', (ev) => {
    ev.stopPropagation();
    pickIconFor(tab);
  });
  el.appendChild(iconBtn);

  const input = document.createElement('input');
  input.className = 'tab-name-input';
  input.value = tab.name;
  input.size = Math.max(tab.name.length, 4);

  const commit = async () => {
    const name = input.value.trim();
    if (!name || name === tab.name) {
      input.value = tab.name;
      return;
    }
    render(await invoke('rename_tab', { id: tab.id, name }));
  };

  input.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') input.blur();
    if (ev.key === 'Escape') {
      input.value = tab.name;
      input.blur();
    }
  });
  input.addEventListener('blur', commit);
  el.appendChild(input);

  const close = document.createElement('span');
  close.className = 'close';
  close.textContent = '✕';
  close.title = `Remove ${tab.name}`;
  // Beat the input's blur so a delete never races a rename round-trip.
  close.addEventListener('mousedown', (ev) => ev.preventDefault());
  close.addEventListener('click', async (ev) => {
    ev.stopPropagation();
    render(await invoke('remove_tab', { id: tab.id }));
  });
  el.appendChild(close);

  return el;
}

editBtn.addEventListener('click', () => {
  editing = !editing;
  render(lastState);
});

reloadBtn.addEventListener('click', async () => {
  // Restart the animation even on rapid repeat clicks.
  reloadBtn.classList.remove('spinning');
  void reloadBtn.offsetWidth;
  reloadBtn.classList.add('spinning');
  try {
    await invoke('reload_active_tab');
  } catch (e) {
    console.error('reload failed', e);
  }
});

function openAddForm() {
  tabsRow.classList.add('hidden');
  addRow.classList.remove('hidden');
  addName.value = '';
  addUrl.value = '';
  addName.focus();
}

function closeAddForm() {
  addRow.classList.add('hidden');
  tabsRow.classList.remove('hidden');
}

addBtn.addEventListener('click', openAddForm);
addCancel.addEventListener('click', closeAddForm);

addConfirm.addEventListener('click', async () => {
  const name = addName.value.trim();
  const url = addUrl.value.trim();
  if (!name || !url) return;
  const next = await invoke('add_tab', { name, url });
  closeAddForm();
  render(next);
});

addUrl.addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') addConfirm.click();
  if (ev.key === 'Escape') closeAddForm();
});
addName.addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') addUrl.focus();
  if (ev.key === 'Escape') closeAddForm();
});

// Tab changes driven from the menu (Ctrl+Tab) happen entirely in the backend,
// so the strip has to be told to repaint its highlight.
window.__TAURI__.event.listen('tabs-changed', (ev) => {
  if (ev.payload) render(ev.payload);
});

invoke('get_state').then(render);
