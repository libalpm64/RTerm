import {
  sessions, activeId, sftpSshId, sftpInitialized,
  setSftpSshId, setSftpInitialized
} from './state.js';

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
      return;
    }

    listEl.innerHTML = '';
    let currentPath = path;

    if (path !== '/') {
      const parentDiv = document.createElement('div');
      parentDiv.className = 'filer-item';
      parentDiv.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span class="name" style="color:var(--text3)">..</span>';
      parentDiv.onclick = () => loadSftpDir(path.split('/').filter(Boolean).slice(0, -1).join('/') || '/');
      listEl.appendChild(parentDiv);
    }

    const files = result.result || [];
    for (const f of files) {
      const div = document.createElement('div');
      div.className = 'filer-item' + (f.dir ? ' selected' : '');
      const icon = f.dir
        ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
        : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
      const sizeStr = f.size ? (f.size > 1024 * 1024 * 1024 ? (f.size / 1024 / 1024 / 1024).toFixed(1) + 'G' : f.size > 1024 * 1024 ? (f.size / 1024 / 1024).toFixed(1) + 'M' : f.size > 1024 ? (f.size / 1024).toFixed(1) + 'K' : f.size + 'B') : '';
      const fullPath = (path === '/' ? '/' : path + '/') + f.name;
      const dlBtn = !f.dir ? `<span class="saction" data-path="${fullPath}" title="Download"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="14" height="14"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg></span>` : '';
      div.innerHTML = `<span class="icon">${icon}</span><span class="name" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:160px">${f.name}</span>${sizeStr ? `<span style="margin-left:auto;padding-right:4px;color:var(--text3);font-size:11px">${sizeStr}</span>` : ''}${dlBtn}`;
      if (f.dir) {
        div.onclick = () => loadSftpDir(fullPath);
      } else {
        div.addEventListener('click', (e) => {
          if (e.target.closest('.saction')) {
            e.stopPropagation();
            let pbar = document.getElementById('dl-progress');
            if (!pbar) {
              pbar = document.createElement('div');
              pbar.id = 'dl-progress';
              pbar.style.cssText = 'position:fixed;bottom:0;left:0;right:0;background:var(--bg3);padding:12px 16px;z-index:9999;border-top:1px solid var(--border);font-size:13px;';
              document.body.appendChild(pbar);
            }
            pbar.innerHTML = `<span>Starting download...</span>`;
            window.rterm.sftpDownload(sftpSshId, fullPath, f.name);
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
          menu.innerHTML = '<div class="dropdown-item" id="sctx-dl">Download to...</div>';
          document.body.appendChild(menu);
          menu.style.left = Math.min(e.clientX, window.innerWidth - 180) + 'px';
          menu.style.top = Math.min(e.clientY, window.innerHeight - 80) + 'px';
          document.getElementById('sctx-dl').onclick = async () => {
            menu.remove();
            const saveDir = await showSaveDialog(f.name);
            if (!saveDir) return;
            let pbar = document.getElementById('dl-progress');
            if (!pbar) {
              pbar = document.createElement('div');
              pbar.id = 'dl-progress';
              pbar.style.cssText = 'position:fixed;bottom:0;left:0;right:0;background:var(--bg3);padding:12px 16px;z-index:9999;border-top:1px solid var(--border);font-size:13px;';
              document.body.appendChild(pbar);
            }
            pbar.innerHTML = `<span>Downloading ${f.name} to ${saveDir}...</span>`;
            window.rterm.sftpDownload(sftpSshId, fullPath, f.name, saveDir);
          };
          setTimeout(() => {
            document.addEventListener('click', () => { const m = document.getElementById('sftp-ctx'); if (m) m.remove(); }, { once: true });
          }, 10);
        };
      }
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

export function showSaveDialog(filename, mode = 'folder') {
  return new Promise((resolve) => {
    _sdResolve = resolve;
    _sdFilename = filename;
    _sdMode = mode;
    document.getElementById('save-dialog').style.display = 'flex';
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
      const parent = dir.split('/').filter(Boolean).slice(0, -1).join('/') || '/';
      p.onclick = () => { _sdPath = parent; loadSaveDir(filename, parent || '/'); };
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
      const full = (dir === '/' ? '/' : dir === '~' ? '~' : dir) + '/' + f.name;
      if (f.dir) {
        d.onclick = () => { _sdPath = full; loadSaveDir(filename, full); };
        d.ondblclick = () => { _sdPath = full; finishSaveDialog(filename); };
      } else {
        d.onclick = () => { _sdPath = full; list.querySelectorAll('.sd-item').forEach(x => x.classList.remove('selected')); d.classList.add('selected'); };
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
