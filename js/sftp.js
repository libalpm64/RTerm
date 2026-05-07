import {
  sessions, activeId, sftpSshId, sftpInitialized, localExpanded,
  setSftpSshId, setSftpInitialized
} from './state.js';

let sftpSelectMode = false;
const sftpSelected = new Map(); // path -> { dir, name }

export async function initSftp(id) {
  if (sftpInitialized) return true;
  const result = await window.rterm.sftpOpen(id);
  if (result.success) {
    setSftpInitialized(true);
    return true;
  }
  console.error('SFTP open failed:', result.error);
  return false;
}

export async function loadSftpDir(path) {
  _currentSftpPath = path || '/';
  const listEl = document.getElementById('sftp-list');
  const pathEl = document.getElementById('sftp-path');
  const statusEl = document.getElementById('sftp-status');
  if (!listEl || !statusEl) return;

  const activeSsh = activeId ? sessions.get(activeId) : null;
  const sess = (activeSsh?.type === 'ssh' && activeSsh.sshId !== null && activeSsh.sshId !== undefined)
    ? activeSsh : Array.from(sessions.values()).find(s => s.type === 'ssh' && s.sshId !== null && s.sshId !== undefined);

  if (!sess || sess.sshId === null || sess.sshId === undefined) {
    listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">No active SSH session</div>';
    return;
  }

  setSftpSshId(sess.sshId);
  statusEl.classList.remove('connected');
  statusEl.classList.add('connecting');
  statusEl.querySelector('.label').textContent = 'Connecting...';

  const ok = await initSftp(sftpSshId);
  if (!ok) {
    statusEl.classList.remove('connecting');
    listEl.innerHTML = '<div style="padding:8px;color:var(--red)">SFTP initialization failed</div>';
    return;
  }

  statusEl.classList.remove('connecting');
  statusEl.classList.add('connected');
  statusEl.querySelector('.label').textContent = 'Connected';

  pathEl.innerHTML = path.split('/').filter(Boolean).map(p => `<span style="color:var(--text3)">/</span><span>${p}</span>`).join('');
  if (path === '/') pathEl.innerHTML = '<span style="color:var(--text3)">/</span>';

  listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">Loading...</div>';

  try {
    const result = await window.rterm.sftpList(sftpSshId, path);
    if (!result.success) {
      listEl.innerHTML = '<div style="color:var(--red);padding:8px">Error: ' + (result.error || 'unknown') + '</div>';
      pathEl.innerHTML = `<input id="sftp-path-input" value="${path.replace(/"/g, '&quot;')}" style="width:100%;background:var(--bg2);border:1px solid var(--border2);color:var(--text);font-size:11px;padding:2px 6px;border-radius:4px;" />`;
      const input = document.getElementById('sftp-path-input');
      if (input) {
        input.addEventListener('keydown', (ev) => {
          if (ev.key === 'Enter') loadSftpDir(input.value || '/');
        });
      }
      return;
    }

    listEl.innerHTML = '';
    ensureSftpBulkBar();
    renderSftpBulkBar();
    let currentPath = path;

    if (path !== '/') {
      const parentDiv = document.createElement('div');
      parentDiv.className = 'filer-item';
      parentDiv.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span class="name" style="color:var(--text3)">..</span>';
      parentDiv.onclick = () => {
        const parent = '/' + path.split('/').filter(Boolean).slice(0, -1).join('/');
        loadSftpDir(parent === '' ? '/' : parent);
      };
      listEl.appendChild(parentDiv);
    }

    const files = result.result || [];
    for (const f of files) {
      const div = document.createElement('div');
      div.className = 'filer-item';
      const icon = f.dir
        ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
        : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
      const sizeStr = f.size ? (f.size > 1024 * 1024 * 1024 ? (f.size / 1024 / 1024 / 1024).toFixed(1) + 'G' : f.size > 1024 * 1024 ? (f.size / 1024 / 1024).toFixed(1) + 'M' : f.size > 1024 ? (f.size / 1024).toFixed(1) + 'K' : f.size + 'B') : '';
      const fullPath = (path === '/' ? '/' : path + '/') + f.name;
      div.innerHTML = `<span class="icon">${icon}</span><span class="name" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:160px">${f.name}</span>${sizeStr ? `<span style="margin-left:auto;padding-right:4px;color:var(--text3);font-size:11px">${sizeStr}</span>` : ''}`;
      if (f.dir) {
        div.onclick = () => {
          if (sftpSelectMode) return toggleSftpSelect(div, fullPath, f);
          loadSftpDir(fullPath);
        };
        div.oncontextmenu = (e) => {
          e.preventDefault();
          e.stopPropagation();
          const old = document.getElementById('sftp-ctx');
          if (old) old.remove();
          const menu = document.createElement('div');
          menu.id = 'sftp-ctx';
          menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:160px;font-size:12px;';
          menu.innerHTML = '<div class="dropdown-item" id="sctx-sm">Select Mode</div>';
          document.body.appendChild(menu);
          menu.style.left = Math.max(4, Math.min(e.clientX, window.innerWidth - 188)) + 'px';
          menu.style.top = Math.max(4, Math.min(e.clientY, window.innerHeight - 88)) + 'px';
          document.getElementById('sctx-sm').onclick = () => {
            menu.remove();
            setSftpSelectMode(true);
            renderSftpBulkBar();
          };
          setTimeout(() => {
            document.addEventListener('click', () => { const m = document.getElementById('sftp-ctx'); if (m) m.remove(); }, { once: true });
          }, 10);
        };
      } else {
        div.addEventListener('click', (e) => {
          if (sftpSelectMode) {
            return toggleSftpSelect(div, fullPath, f);
          }
        });
        div.oncontextmenu = (e) => {
          e.preventDefault();
          e.stopPropagation();
          const old = document.getElementById('sftp-ctx');
          if (old) old.remove();
          const menu = document.createElement('div');
          menu.id = 'sftp-ctx';
          menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:160px;font-size:12px;';
          menu.innerHTML = '<div class="dropdown-item" id="sctx-dl">Download to...</div><div class="dropdown-item" id="sctx-sm">Select Mode</div>';
          document.body.appendChild(menu);
          menu.style.left = Math.max(4, Math.min(e.clientX, window.innerWidth - 188)) + 'px';
          menu.style.top = Math.max(4, Math.min(e.clientY, window.innerHeight - 88)) + 'px';
          document.getElementById('sctx-dl').onclick = async () => {
            menu.remove();
            const saveDir = await showSaveDialog(f.name);
            if (!saveDir) return;
            let pbar = document.getElementById('dl-progress');
            if (!pbar) {
              pbar = document.createElement('div');
              pbar.id = 'dl-progress';
              pbar.style.cssText = 'position:fixed;top:38px;right:12px;min-width:280px;max-width:480px;background:var(--bg3);padding:10px 12px;z-index:9999;border:1px solid var(--border2);border-radius:8px;box-shadow:0 8px 24px rgba(0,0,0,.4);font-size:12px;';
              document.body.appendChild(pbar);
            }
            pbar.innerHTML = `<span>Downloading ${f.name} to ${saveDir}...</span>`;
            window.rterm.sftpDownload(sftpSshId, fullPath, f.name, saveDir);
          };
          document.getElementById('sctx-sm').onclick = () => {
            menu.remove();
            setSftpSelectMode(true);
            renderSftpBulkBar();
          };
          setTimeout(() => {
            document.addEventListener('click', () => { const m = document.getElementById('sftp-ctx'); if (m) m.remove(); }, { once: true });
          }, 10);
        };
      }
      if (sftpSelected.has(fullPath)) div.classList.add('selected');
      listEl.appendChild(div);
    }
  } catch (e) {
    listEl.innerHTML = '<div style="color:var(--red);padding:8px">Error: ' + e + '</div>';
  }
}

let _sdResolve = null;
let _sdPath = '~';
let _sdFilename = '';
let _sdMode = 'folder';
let _currentSftpPath = '/';

export function getCurrentSftpPath() {
  return _currentSftpPath;
}

export function showSaveDialog(filename, mode = 'folder') {
  return new Promise((resolve) => {
    _sdResolve = resolve;
    _sdFilename = filename;
    _sdMode = mode;
    if (mode === 'file' && localExpanded) _sdPath = localExpanded;
    if (mode === 'folder' && (!_sdPath || _sdPath === '~') && localExpanded) _sdPath = localExpanded;
    document.getElementById('save-dialog').style.display = 'flex';
    const header = document.getElementById('sd-header-title');
    if (header) header.textContent = mode === 'file' ? 'Select Local File to Upload' : 'Select Download Folder';
    document.getElementById('sd-title').textContent = mode === 'file' ? 'Select File to Upload' : 'Select Destination Folder';
    document.getElementById('sd-select').textContent = mode === 'file' ? 'Select File' : 'Select Folder';
    loadSaveDir(filename, _sdPath);
  });
}

function loadSaveDir(filename, dir) {
  const list = document.getElementById('sd-list');
  const pathEl = document.getElementById('sd-path');
  _sdPath = dir;
  pathEl.textContent = dir === '~' ? '/home/' + (document.getElementById('ssh-user')?.value || 'user') : dir;
  list.innerHTML = '<div style="padding:16px;color:var(--text3);text-align:center;font-size:13px;">Loading...</div>';

  window.rterm.localList(dir).then(result => {
    if (!result || !result.success) {
      list.innerHTML = '<div style="padding:16px;color:var(--red);text-align:center;font-size:13px;">' + (result?.error || 'Cannot read directory') + '</div>';
      return;
    }
    const files = result.result || [];
    list.innerHTML = '';

    if (dir !== '/' && dir !== '~') {
      const p = document.createElement('div');
      p.className = 'sd-item';
      p.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span>..</span>';
      p.onclick = () => {
        const parent = '/' + dir.split('/').filter(Boolean).slice(0, -1).join('/');
        _sdPath = parent === '' ? '/' : parent;
        loadSaveDir(filename, _sdPath);
      };
      list.appendChild(p);
    }

    let hasItems = false;
    for (const f of files) {
      if (!f.dir && _sdMode === 'folder') continue;
      hasItems = true;
      const d = document.createElement('div');
      d.className = 'sd-item';
      const icon = f.dir
        ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
        : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
      d.innerHTML = `<span class="icon">${icon}</span><span>${f.name}</span>`;
      const full = (dir === '/' ? '' : dir === '~' ? '~' : dir) + '/' + f.name;
      if (f.dir) {
        d.onclick = () => { _sdPath = full; loadSaveDir(filename, full); };
        d.ondblclick = () => { _sdPath = full; finishSaveDialog(filename); };
      } else {
        d.onclick = () => {
          _sdPath = full;
          list.querySelectorAll('.sd-item').forEach(x => x.classList.remove('selected'));
          d.classList.add('selected');
        };
        d.ondblclick = () => { _sdPath = full; finishSaveDialog(filename); };
      }
      list.appendChild(d);
    }
    if (!hasItems && dir !== '/') {
      list.innerHTML += '<div style="padding:16px;color:var(--text3);text-align:center;font-size:12px;">(empty directory)</div>';
    }
  });
}

function finishSaveDialog(filename) {
  let path = _sdPath;
  if (path === '~') {
    path = window.__rterm_home || '/tmp';
  } else if (_sdMode === 'file') {
    document.getElementById('save-dialog').style.display = 'none';
    if (_sdResolve) { _sdResolve(path); _sdResolve = null; }
    return;
  }
  document.getElementById('save-dialog').style.display = 'none';
  if (_sdResolve) { _sdResolve(path); _sdResolve = null; }
}

export function setupSaveDialog() {
  document.getElementById('sd-close').onclick = () => {
    document.getElementById('save-dialog').style.display = 'none';
    if (_sdResolve) { _sdResolve(null); _sdResolve = null; }
  };
  document.getElementById('sd-cancel').onclick = () => {
    document.getElementById('save-dialog').style.display = 'none';
    if (_sdResolve) { _sdResolve(null); _sdResolve = null; }
  };
  document.getElementById('sd-select').onclick = () => {
    if (_sdMode === 'file') {
      const selected = document.querySelector('#sd-list .sd-item.selected');
      if (!selected) return;
    }
    finishSaveDialog(_sdFilename);
  };
}

window.loadSftpDir = loadSftpDir;

function toggleSftpSelect(el, path, fileMeta) {
  if (sftpSelected.has(path)) {
    sftpSelected.delete(path);
    el.classList.remove('selected');
    el.style.background = '';
    el.style.color = '';
  } else {
    sftpSelected.set(path, { dir: !!fileMeta.dir, name: fileMeta.name });
    el.classList.add('selected');
    if (sftpSelectMode) {
      el.style.background = 'rgba(255,36,55,.18)';
      el.style.color = 'var(--red)';
    }
  }
  renderSftpBulkBar();
}

function ensureSftpBulkBar() {
  if (document.getElementById('sftp-bulk-bar')) return;
  const container = document.getElementById('panel-sftp');
  const bar = document.createElement('div');
  bar.id = 'sftp-bulk-bar';
  bar.style.cssText = 'display:none;gap:8px;padding:6px 8px;border-top:1px solid var(--border);background:var(--bg3);align-items:center;font-size:11px;';
  bar.innerHTML = `<span id="sftp-bulk-count" style="color:var(--text2)"></span><button id="sftp-bulk-exit" class="btn btn-cancel" style="padding:4px 8px;margin-left:auto;">Cancel</button>`;
  container.appendChild(bar);
  ensureSftpTopActions();

  document.getElementById('sftp-bulk-exit').onclick = () => {
    setSftpSelectMode(false);
  };

}

function ensureSftpTopActions() {
  if (document.getElementById('sftp-bulk-download-top')) return;
  const icons = document.querySelector('#panel-sftp .panel-icons');
  if (!icons) return;
  const dl = document.createElement('span');
  dl.id = 'sftp-bulk-download-top';
  dl.className = 'panel-icon';
  dl.title = 'Download Selected';
  dl.style.cssText = 'display:none;';
  dl.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>';
  const mv = document.createElement('span');
  mv.id = 'sftp-bulk-move-top';
  mv.className = 'panel-icon';
  mv.title = 'Move Selected';
  mv.style.cssText = 'display:none;';
  mv.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="5 9 2 12 5 15"/><polyline points="9 5 12 2 15 5"/><polyline points="15 19 12 22 9 19"/><polyline points="19 9 22 12 19 15"/><line x1="2" y1="12" x2="22" y2="12"/><line x1="12" y1="2" x2="12" y2="22"/></svg>';
  const del = document.createElement('span');
  del.id = 'sftp-bulk-delete-top';
  del.className = 'panel-icon';
  del.title = 'Delete Selected';
  del.style.cssText = 'display:none;color:var(--red);';
  del.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>';
  icons.insertBefore(del, document.getElementById('sftp-refresh'));
  icons.insertBefore(mv, del);
  icons.insertBefore(dl, mv);
  dl.onclick = async () => {
    const entries = Array.from(sftpSelected.entries()).filter(([, v]) => !v.dir);
    for (const [p, meta] of entries) {
      window.rterm.sftpDownload(sftpSshId, p, meta.name);
    }
  };
  mv.onclick = async () => {
    const targetDir = prompt('Move selected remote items to directory:', getCurrentSftpPath() || '/');
    if (!targetDir) return;
    const sess = sessions.get(activeId);
    if (!sess?.sshId) return;
    for (const [p, meta] of Array.from(sftpSelected.entries())) {
      const cmd = `mv ${shellQuote(p)} ${shellQuote((targetDir === '/' ? '' : targetDir) + '/' + meta.name)}`;
      await window.rterm.sshExec(sess.sshId, cmd);
    }
    sftpSelected.clear();
    renderSftpBulkBar();
    loadSftpDir(getCurrentSftpPath() || '/');
  };

  del.onclick = async () => {
    const sess = sessions.get(activeId);
    if (!sess?.sshId) return;
    for (const [p] of Array.from(sftpSelected.entries())) {
      await window.rterm.sshExec(sess.sshId, `rm -rf ${shellQuote(p)}`);
    }
    sftpSelected.clear();
    renderSftpBulkBar();
    loadSftpDir(getCurrentSftpPath() || '/');
  };
}

function renderSftpBulkBar() {
  const bar = document.getElementById('sftp-bulk-bar');
  const count = document.getElementById('sftp-bulk-count');
  if (!bar || !count) return;
  bar.style.display = sftpSelectMode ? 'flex' : 'none';
  count.textContent = `${sftpSelected.size} selected`;
  const dl = document.getElementById('sftp-bulk-download-top');
  const mv = document.getElementById('sftp-bulk-move-top');
  const del = document.getElementById('sftp-bulk-delete-top');
  if (dl) dl.style.display = sftpSelectMode ? '' : 'none';
  if (mv) mv.style.display = sftpSelectMode ? '' : 'none';
  if (del) del.style.display = sftpSelectMode ? '' : 'none';
  const cancelBtn = document.getElementById('sftp-bulk-exit');
  if (cancelBtn) {
    cancelBtn.style.boxShadow = sftpSelectMode ? '0 0 0 1px rgba(79,195,247,.45), 0 0 10px rgba(79,195,247,.35)' : '';
  }
}

export function setSftpSelectMode(enabled) {
  sftpSelectMode = enabled;
  if (!enabled) {
    sftpSelected.clear();
    document.querySelectorAll('#sftp-list .filer-item.selected').forEach(el => {
      el.classList.remove('selected');
      el.style.background = '';
      el.style.color = '';
    });
  }
  renderSftpBulkBar();
}

function shellQuote(s) {
  return `'${String(s).replace(/'/g, `'\\''`)}'`;
}
