import { localPath, localExpanded, localHome, setLocalPath, setLocalExpanded, setLocalHome } from './state.js';
import { toggleSidebar } from './terminal.js';

let pendingLocalDelete = null;
let localSelectMode = false;
const localSelected = new Set();
const localTypeMap = new Map();
let localRowMap = new Map();
let localActionModalResolver = null;
let localPickerResolve = null;
let localPrevBodyOverflow = '';
let localPrevHtmlOverflow = '';

function setLocalPageScrollLock(locked) {
  if (locked) {
    localPrevBodyOverflow = document.body.style.overflow || '';
    localPrevHtmlOverflow = document.documentElement.style.overflow || '';
    document.body.style.overflow = 'hidden';
    document.documentElement.style.overflow = 'hidden';
  } else {
    document.body.style.overflow = localPrevBodyOverflow;
    document.documentElement.style.overflow = localPrevHtmlOverflow;
  }
}

function ensureLocalDeleteModal() {
  if (document.getElementById('local-delete-modal')) return;
  const modal = document.createElement('div');
  modal.id = 'local-delete-modal';
  modal.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.6);backdrop-filter:blur(2px);display:none;align-items:center;justify-content:center;z-index:6000;';
  modal.innerHTML = `
    <div style="width:520px;max-width:92vw;background:var(--bg2);border:1px solid var(--border2);border-radius:8px;box-shadow:0 12px 36px rgba(0,0,0,.5);">
      <div style="padding:14px 16px;border-bottom:1px solid var(--border);font-size:14px;font-weight:600;color:var(--text);">Delete Item</div>
      <div style="padding:14px 16px;line-height:1.5;color:var(--text2);font-size:12px;">
        <div id="local-delete-summary" style="color:var(--text);margin-bottom:10px;"></div>
        <div id="local-delete-path" style="font-family:'JetBrains Mono',monospace;color:var(--text2);word-break:break-all;"></div>
        <div style="margin-top:10px;color:var(--red);">This action is permanent and cannot be undone.</div>
      </div>
      <div style="display:flex;justify-content:flex-end;gap:8px;padding:12px 16px;border-top:1px solid var(--border);">
        <button id="local-delete-cancel" class="btn btn-cancel">Cancel</button>
        <button id="local-delete-confirm" class="btn btn-danger">Delete</button>
      </div>
    </div>`;
  document.body.appendChild(modal);

  document.getElementById('local-delete-cancel').onclick = () => {
    pendingLocalDelete = null;
    modal.style.display = 'none';
  };
  document.getElementById('local-delete-confirm').onclick = async () => {
    if (!pendingLocalDelete) return;
    const { path, isDir } = pendingLocalDelete;
    modal.style.display = 'none';
    pendingLocalDelete = null;
    const result = await window.rterm.localDelete(path, isDir);
    if (result.success) loadLocalDir(localPath);
    else alert('Delete failed: ' + (result.error || 'unknown'));
  };
  modal.addEventListener('click', (e) => {
    if (e.target === modal) {
      pendingLocalDelete = null;
      modal.style.display = 'none';
    }
  });
}

function openLocalDeleteModal(name, path, isDir) {
  ensureLocalDeleteModal();
  pendingLocalDelete = { path, isDir };
  const kind = isDir ? 'folder' : 'file';
  document.getElementById('local-delete-summary').textContent = `Delete ${kind}: ${name}`;
  document.getElementById('local-delete-path').textContent = path;
  document.getElementById('local-delete-modal').style.display = 'flex';
}

function ensureLocalActionModal() {
  if (document.getElementById('local-action-modal')) return;
  const modal = document.createElement('div');
  modal.id = 'local-action-modal';
  modal.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.6);backdrop-filter:blur(2px);display:none;align-items:center;justify-content:center;z-index:7000;';
  modal.innerHTML = `
    <div style="width:560px;max-width:92vw;background:var(--bg2);border:1px solid var(--border2);border-radius:8px;box-shadow:0 12px 36px rgba(0,0,0,.5);">
      <div id="local-action-title" style="padding:12px 16px;border-bottom:1px solid var(--border);font-size:14px;font-weight:600;color:var(--text);">Action</div>
      <div id="local-action-body" style="padding:12px 16px;color:var(--text2);font-size:12px;line-height:1.55;"></div>
      <div id="local-action-input-wrap" style="padding:0 16px 12px 16px;display:none;">
        <input id="local-action-input" type="text" style="width:100%;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:8px;color:var(--text);font-size:12px;">
      </div>
      <div style="display:flex;justify-content:flex-end;gap:8px;padding:10px 16px;border-top:1px solid var(--border);">
        <button id="local-action-cancel" class="btn btn-cancel">Cancel</button>
        <button id="local-action-ok" class="btn btn-primary">OK</button>
      </div>
    </div>`;
  document.body.appendChild(modal);
  const close = () => {
    modal.style.display = 'none';
    if (localActionModalResolver) { localActionModalResolver(null); localActionModalResolver = null; }
  };
  document.getElementById('local-action-cancel').onclick = close;
  modal.addEventListener('click', (e) => { if (e.target === modal) close(); });
}

function openLocalActionModal({ title, body, inputValue = '', withInput = false, okText = 'OK', showCancel = true }) {
  ensureLocalActionModal();
  const modal = document.getElementById('local-action-modal');
  const titleEl = document.getElementById('local-action-title');
  const bodyEl = document.getElementById('local-action-body');
  const inputWrap = document.getElementById('local-action-input-wrap');
  const inputEl = document.getElementById('local-action-input');
  const okBtn = document.getElementById('local-action-ok');
  const cancelBtn = document.getElementById('local-action-cancel');
  titleEl.textContent = title;
  bodyEl.innerHTML = body;
  okBtn.textContent = okText;
  inputWrap.style.display = withInput ? 'block' : 'none';
  inputEl.value = inputValue || '';
  cancelBtn.style.display = showCancel ? '' : 'none';
  modal.style.display = 'flex';
  if (withInput) setTimeout(() => inputEl.focus(), 0);
  if (withInput) {
    bodyEl.querySelectorAll('.local-dir-pick').forEach((el) => {
      el.addEventListener('click', () => {
        inputEl.value = el.getAttribute('data-dir') || '/';
      });
    });
  }
  return new Promise((resolve) => {
    localActionModalResolver = resolve;
    okBtn.onclick = () => {
      const val = withInput ? inputEl.value.trim() : true;
      modal.style.display = 'none';
      localActionModalResolver = null;
      resolve(val);
    };
  });
}

function permToRwx(perm) {
  const n = Number(perm);
  if (!Number.isFinite(n) || n === 0) return 'unknown';
  const v = n & 0o777;
  const tri = (x) => `${x & 4 ? 'r' : '-'}${x & 2 ? 'w' : '-'}${x & 1 ? 'x' : '-'}`;
  return tri((v >> 6) & 7) + tri((v >> 3) & 7) + tri(v & 7);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function buildLocalMoveFolderList(files, currentPath) {
  const dirs = (files || []).filter(x => x.dir).map(x => x.name).slice(0, 200);
  const up = currentPath !== '/' ? `<div class="local-dir-pick" data-dir=".." style="padding:5px 8px;border-radius:4px;cursor:pointer;color:var(--text2);">..</div>` : '';
  if (!dirs.length && !up) return '<div style="margin-top:8px;color:var(--text3)">(no subfolders)</div>';
  return `<div style="margin-top:8px;max-height:220px;overflow:auto;border:1px solid var(--border2);border-radius:6px;padding:4px;background:var(--bg3);">${up}${dirs.map(d => {
    const target = (currentPath === '/' ? '' : currentPath) + '/' + d;
    return `<div class="local-dir-pick" data-dir="${escapeHtml(target)}" style="padding:5px 8px;border-radius:4px;cursor:pointer;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--text2);">${escapeHtml(d)}</div>`;
  }).join('')}</div>`;
}

async function pickLocalTargetDir(initialPath, title, itemName) {
  if (!document.getElementById('local-move-picker')) {
    const m = document.createElement('div');
    m.id = 'local-move-picker';
    m.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.62);backdrop-filter:blur(2px);display:none;align-items:center;justify-content:center;z-index:8100;';
    m.innerHTML = `<div style="width:680px;max-width:94vw;height:520px;max-height:86vh;background:var(--bg2);border:1px solid var(--border2);border-radius:8px;display:flex;flex-direction:column;overflow:hidden;">
      <div style="padding:12px 14px;border-bottom:1px solid var(--border);font-size:14px;font-weight:600;color:var(--text);" id="local-move-title"></div>
      <div style="padding:8px 14px;color:var(--text3);font-family:monospace;font-size:11px;border-bottom:1px solid var(--border);" id="local-move-path"></div>
      <div id="local-move-list" style="flex:1;min-height:0;overflow-y:auto;overflow-x:hidden;padding:8px 10px;scrollbar-width:none;"></div>
      <div style="display:flex;gap:8px;justify-content:flex-end;padding:10px 14px;border-top:1px solid var(--border);">
        <div style="display:flex;gap:8px;">
          <button id="local-move-cancel" class="btn btn-cancel">Cancel</button>
          <button id="local-move-here" class="btn btn-primary">Move Here</button>
        </div>
      </div>
    </div>`;
    document.body.appendChild(m);
    m.addEventListener('click', (e) => {
      if (e.target === m && localPickerResolve) { localPickerResolve(null); localPickerResolve = null; m.style.display = 'none'; }
    });
  }
  const modal = document.getElementById('local-move-picker');
  const titleEl = document.getElementById('local-move-title');
  const pathEl = document.getElementById('local-move-path');
  const listEl = document.getElementById('local-move-list');
  const cancelBtn = document.getElementById('local-move-cancel');
  const hereBtn = document.getElementById('local-move-here');
  let path = initialPath || '/';
  let selectedDir = null;
  const render = async () => {
    titleEl.textContent = `${title}: ${itemName}`;
    pathEl.textContent = path;
    listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">Loading...</div>';
    const res = await window.rterm.localList(path);
    if (!res?.success) { listEl.innerHTML = `<div style="padding:8px;color:var(--red)">${escapeHtml(res?.error || 'list failed')}</div>`; return; }
    const dirs = (res.result || []).filter(x => x.dir);
    selectedDir = null;
    listEl.innerHTML = '';
    if (path !== '/') {
      const up = document.createElement('div');
      up.style.cssText = 'display:flex;align-items:center;gap:8px;padding:6px 8px;border-radius:6px;cursor:pointer;color:var(--text2);';
      up.innerHTML = `<span style="width:14px;height:14px;display:inline-flex;"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.6"><path d="M3 12h18"></path><path d="M9 6l-6 6 6 6"></path></svg></span><span>.../</span>`;
      up.onclick = async () => { path = '/' + path.split('/').filter(Boolean).slice(0, -1).join('/'); if (path === '') path = '/'; await render(); };
      listEl.appendChild(up);
    }
    if (!dirs.length && path === '/') { listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">(empty)</div>'; return; }
    for (const d of dirs) {
      const full = (path === '/' ? '' : path) + '/' + d.name;
      const row = document.createElement('div');
      row.style.cssText = 'display:flex;align-items:center;gap:8px;padding:6px 8px;border-radius:6px;cursor:pointer;color:var(--text2);';
      row.innerHTML = `<span style="width:14px;height:14px;display:inline-flex;"><svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.6"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg></span><span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">${escapeHtml(d.name)}</span>`;
      row.onmouseenter = () => { if (selectedDir !== full) row.style.background = 'var(--bg4)'; };
      row.onmouseleave = () => { if (selectedDir !== full) row.style.background = 'transparent'; };
      row.onclick = () => {
        selectedDir = full;
        Array.from(listEl.children).forEach(c => { c.style.background = 'transparent'; c.style.color = 'var(--text2)'; });
        row.style.background = 'rgba(79,195,247,.18)';
        row.style.color = 'var(--text)';
      };
      row.ondblclick = async () => { path = full; await render(); };
      listEl.appendChild(row);
    }
  };
  await render();
  modal.style.display = 'flex';
  setLocalPageScrollLock(true);
  return await new Promise((resolve) => {
    localPickerResolve = resolve;
    cancelBtn.onclick = () => { modal.style.display = 'none'; setLocalPageScrollLock(false); localPickerResolve = null; resolve(null); };
    hereBtn.onclick = () => { const out = selectedDir || path; modal.style.display = 'none'; setLocalPageScrollLock(false); localPickerResolve = null; resolve(out); };
  });
}

export async function loadLocalDir(dir) {
  const listEl = document.getElementById('filer-list');
  const pathEl = document.getElementById('filer-path');
  if (!listEl) return;

  if (!localHome) {
    const h = await window.rterm.localList('~');
    setLocalHome((h?.home || h?.path || '').trim() || '~');
  }

  const resolved = dir === '~' || dir === '' ? localHome : dir;
  setLocalExpanded(resolved);
  setLocalPath(dir);

  const result = await window.rterm.localList(resolved);
  if (!result.success) { listEl.innerHTML = '<div style="padding:8px;color:var(--red)">Error: ' + (result.error || 'unknown') + '</div>'; return; }
  if (result.home) setLocalHome(result.home);

  const parts = dir.split('/').filter(Boolean);
  pathEl.innerHTML = parts.map((p, i) => {
    const path = '/' + parts.slice(0, i + 1).join('/');
    return `<span style="color:var(--text3)">/</span><span onclick="loadLocalDir('${path}')">${p}</span>`;
  }).join('');
  if (dir === '/') pathEl.innerHTML = '<span style="color:var(--text3)">/</span>';
  else if (dir === '~') pathEl.innerHTML = '<span style="color:var(--text3)">~</span>';

  listEl.innerHTML = '';
  localRowMap = new Map();
  // Prune metadata for items no longer selected
  for (const key of localTypeMap.keys()) {
    if (!localSelected.has(key)) localTypeMap.delete(key);
  }
  ensureLocalBulkBar();
  renderLocalBulkBar();

  if (dir !== '/' && dir !== '~') {
    const parentDiv = document.createElement('div');
    parentDiv.className = 'filer-item';
    parentDiv.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span class="name" style="color:var(--text3)">..</span>';
    parentDiv.onclick = () => loadLocalDir(dir.split('/').filter(Boolean).slice(0, -1).join('/') || '/');
    listEl.appendChild(parentDiv);
  }

  const files = result.result || [];
  files.forEach(f => {
    const div = document.createElement('div');
    div.className = 'filer-item';
    const icon = f.dir
      ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
      : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
    const sizeStr = f.size ? (f.size > 1024 * 1024 * 1024 ? (f.size / 1024 / 1024 / 1024).toFixed(1) + 'G' : f.size > 1024 * 1024 ? (f.size / 1024 / 1024).toFixed(1) + 'M' : f.size > 1024 ? (f.size / 1024).toFixed(1) + 'K' : f.size + 'B') : '';
    div.innerHTML = `<span class="icon">${icon}</span><span class="name">${escapeHtml(f.name)}</span>${sizeStr ? `<span class="size">${sizeStr}</span>` : ''}`;
    const base = localExpanded === '/' ? '' : localExpanded;
    const fullPath = base + '/' + f.name;
    const key = fullPath;
    localTypeMap.set(key, Boolean(f.dir));
    localRowMap.set(key, div);
    if (localSelected.has(key)) div.classList.add('selected');
    if (f.dir) {
      div.addEventListener('click', (e) => {
        if (localSelectMode) {
          toggleLocalSelect(div, key);
          return;
        }
        if (e.button !== 0 || e.ctrlKey || e.metaKey) return;
        loadLocalDir((dir === '/' ? '/' : dir + '/') + f.name);
      });
    } else {
      div.addEventListener('click', () => {
        if (localSelectMode) toggleLocalSelect(div, key);
      });
    }
    div.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      e.stopPropagation();
      const old = document.getElementById('local-ctx');
      if (old) old.remove();
      const menu = document.createElement('div');
      menu.id = 'local-ctx';
      menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:140px;font-size:12px;';
      const deleteItem = document.createElement('div');
      const moveItem = document.createElement('div');
      const renameItem = document.createElement('div');
      const infoItem = document.createElement('div');
      deleteItem.className = 'dropdown-item';
      moveItem.className = 'dropdown-item';
      renameItem.className = 'dropdown-item';
      infoItem.className = 'dropdown-item';
      deleteItem.style.color = 'var(--red)';
      moveItem.textContent = 'Move...';
      renameItem.textContent = 'Rename...';
      infoItem.textContent = 'Info';
      deleteItem.textContent = 'Delete ' + (f.dir ? 'Folder' : 'File');
      const selectModeItem = document.createElement('div');
      selectModeItem.className = 'dropdown-item';
      selectModeItem.textContent = localSelectMode ? 'Exit Select Mode' : 'Select Mode';
      menu.appendChild(moveItem);
      menu.appendChild(renameItem);
      menu.appendChild(infoItem);
      menu.appendChild(deleteItem);
      menu.appendChild(selectModeItem);
      document.body.appendChild(menu);
      menu.addEventListener('mousedown', (ev) => ev.stopPropagation());
      menu.addEventListener('click', (ev) => ev.stopPropagation());
      menu.style.left = Math.min(e.clientX, window.innerWidth - 160) + 'px';
      menu.style.top = Math.min(e.clientY, window.innerHeight - 60) + 'px';
      moveItem.onclick = async (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        menu.remove();
        const targetDir = await pickLocalTargetDir(localExpanded || '/', 'Move Item', f.name);
        if (!targetDir) return;
        const name = f.name;
        const toPath = (targetDir === '/' ? '' : targetDir) + '/' + name;
        const result = await window.rterm.localMove(fullPath, toPath);
        if (!result?.success) {
          await openLocalActionModal({ title: 'Move Failed', body: String(result?.error || 'unknown'), withInput: false, okText: 'Close' });
        }
        else loadLocalDir(localPath);
      };
      renameItem.onclick = async (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        menu.remove();
        const newName = await openLocalActionModal({
          title: 'Rename Item',
          body: `Rename <b>${escapeHtml(f.name)}</b> to:`,
          inputValue: f.name,
          withInput: true,
          okText: 'Rename'
        });
        if (!newName || newName === f.name) return;
        const baseDir = fullPath.split('/').slice(0, -1).join('/') || '/';
        const toPath = (baseDir === '/' ? '' : baseDir) + '/' + newName;
        const result = await window.rterm.localMove(fullPath, toPath);
        if (!result?.success) {
          await openLocalActionModal({ title: 'Rename Failed', body: String(result?.error || 'unknown'), withInput: false, okText: 'Close' });
        }
        else loadLocalDir(localPath);
      };
      infoItem.onclick = (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        menu.remove();
        openLocalActionModal({
          title: 'File Info',
          body:
            `Name: ${escapeHtml(f.name)}<br>` +
            `Type: ${f.dir ? 'Folder' : 'File'}<br>` +
            `Permissions: ${permToRwx(f.perm)}<br>` +
            `Size: ${f.size || 0} bytes<br>` +
            `Path: ${escapeHtml(fullPath)}`,
          withInput: false,
          okText: 'Close',
          showCancel: false
        });
      };
      deleteItem.onmousedown = (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
      };
      deleteItem.onclick = async (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        menu.remove();
        openLocalDeleteModal(f.name, fullPath, f.dir);
      };
      selectModeItem.onclick = (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        menu.remove();
        setLocalSelectMode(!localSelectMode);
      };
      requestAnimationFrame(() => {
        const closeMenu = (ev) => {
          const m = document.getElementById('local-ctx');
          if (!m) return;
          if (m.contains(ev.target)) return;
          m.remove();
          document.removeEventListener('pointerdown', closeMenu, true);
        };
        document.addEventListener('pointerdown', closeMenu, true);
      });
    });
    listEl.appendChild(div);
  });
}

window.loadLocalDir = loadLocalDir;

function toggleLocalSelect(el, key) {
  if (localSelected.has(key)) {
    localSelected.delete(key);
    localTypeMap.delete(key);
    el.classList.remove('selected');
    el.style.background = '';
    el.style.color = '';
  } else {
    localSelected.add(key);
    el.classList.add('selected');
    if (localSelectMode) {
      el.style.background = 'rgba(255,36,55,.18)';
      el.style.color = 'var(--red)';
    }
  }
  renderLocalBulkBar();
}

function ensureLocalBulkBar() {
  if (document.getElementById('local-bulk-bar')) return;
  const container = document.getElementById('panel-local');
  const bar = document.createElement('div');
  bar.id = 'local-bulk-bar';
  bar.style.cssText = 'display:none;gap:8px;padding:6px 8px;border-top:1px solid var(--border);background:var(--bg3);align-items:center;font-size:11px;';
  bar.innerHTML = `<span id="local-bulk-count" style="color:var(--text2)"></span><button id="local-bulk-exit" class="btn btn-cancel" style="padding:4px 8px;margin-left:auto;">Cancel</button>`;
  container.appendChild(bar);
  ensureLocalTopActions();
  document.getElementById('local-bulk-exit').onclick = () => {
    setLocalSelectMode(false);
  };
}

function ensureLocalTopActions() {
  if (document.getElementById('local-bulk-delete-top')) return;
  const icons = document.querySelector('#panel-local .panel-icons');
  if (!icons) return;
  const del = document.createElement('span');
  del.id = 'local-bulk-delete-top';
  del.className = 'panel-icon';
  del.title = 'Delete Selected';
  del.style.cssText = 'display:none;color:var(--red);';
  del.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>';
  const mv = document.createElement('span');
  mv.id = 'local-bulk-move-top';
  mv.className = 'panel-icon';
  mv.title = 'Move Selected';
  mv.style.cssText = 'display:none;';
  mv.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="5 9 2 12 5 15"/><polyline points="9 5 12 2 15 5"/><polyline points="15 19 12 22 9 19"/><polyline points="19 9 22 12 19 15"/><line x1="2" y1="12" x2="22" y2="12"/><line x1="12" y1="2" x2="12" y2="22"/></svg>';
  icons.insertBefore(mv, document.getElementById('filer-toggle'));
  icons.insertBefore(del, mv);
  del.onclick = async () => {
    const items = Array.from(localSelected);
    for (const p of items) {
      const isDir = localTypeMap.get(p) || false;
      await window.rterm.localDelete(p, isDir);
    }
    localSelected.clear();
    localTypeMap.clear();
    renderLocalBulkBar();
    loadLocalDir(localPath);
  };
  mv.onclick = async () => {
    const targetDir = await pickLocalTargetDir(localExpanded || '/', 'Move Selected Items', `${localSelected.size} item(s)`);
    if (!targetDir) return;
    for (const p of Array.from(localSelected)) {
      const name = p.split('/').pop();
      await window.rterm.localMove(p, (targetDir === '/' ? '' : targetDir) + '/' + name);
    }
    localSelected.clear();
    localTypeMap.clear();
    renderLocalBulkBar();
    loadLocalDir(localPath);
  };
}

function renderLocalBulkBar() {
  const bar = document.getElementById('local-bulk-bar');
  const count = document.getElementById('local-bulk-count');
  if (!bar || !count) return;
  bar.style.display = localSelectMode ? 'flex' : 'none';
  count.textContent = `${localSelected.size} selected`;
  const del = document.getElementById('local-bulk-delete-top');
  const mv = document.getElementById('local-bulk-move-top');
  if (del) del.style.display = localSelectMode ? '' : 'none';
  if (mv) mv.style.display = localSelectMode ? '' : 'none';
  const cancelBtn = document.getElementById('local-bulk-exit');
  if (cancelBtn) {
    cancelBtn.style.boxShadow = localSelectMode ? '0 0 0 1px rgba(79,195,247,.45), 0 0 10px rgba(79,195,247,.35)' : '';
  }
}

function setLocalSelectMode(enabled) {
  localSelectMode = enabled;
  if (!enabled) {
    for (const key of localSelected) {
      const el = localRowMap.get(key);
      if (!el) continue;
      el.classList.remove('selected');
      el.style.background = '';
      el.style.color = '';
    }
    localSelected.clear();
    localTypeMap.clear();
  }
  renderLocalBulkBar();
}
