import { semanticHlEnabled, activeKeywords } from './state.js';

const ANSI = {
  red: '\x1b[1;31m',
  yellow: '\x1b[1;33m',
  green: '\x1b[1;32m',
  cyan: '\x1b[1;36m',
  magenta: '\x1b[1;35m',
  blue: '\x1b[1;34m',
  orange: '\x1b[38;5;214m',
  gray: '\x1b[38;5;244m',
  reset: '\x1b[0m',
};

const STATIC_RULES = semanticHlEnabled ? [
  { re: /\b(sudo|rm|dd|mkfs|shutdown|reboot|halt|poweroff)\b/g, color: ANSI.red },
  { re: /\b(ls|cd|grep|awk|sed|cat|less|more|tail|head|find|ssh|scp|git|docker|npm|cargo|pip|python|gcc|make|apt|yum|dnf|systemctl|journalctl)\b/g, color: ANSI.yellow },
  { re: /\b(\d{1,3}(?:\.\d{1,3}){3})\b/g, color: ANSI.magenta },
  { re: /(\/[a-zA-Z0-9._\-\/]+)/g, color: ANSI.cyan },
  { re: /\b([drwx-]{10})\b/g, color: ANSI.cyan },
  { re: /(\$[a-zA-Z_][a-zA-Z0-9_]*)\b/g, color: ANSI.cyan },
] : [];

function applyStaticRules(str) {
  for (const { re, color } of STATIC_RULES) {
    str = str.replace(re, `${color}$1${ANSI.reset}`);
  }
  return str;
}

export function applyHighlighting(data) {
  if (!data || typeof data !== 'string') return data;
  if (data.length > 20000) return data;

  let res = data;

  if (semanticHlEnabled) {
    res = applyStaticRules(res);
  }

  if (activeKeywords.length > 0) {
    // Build ONE regex instead of N regexes
    const escaped = activeKeywords
      .filter(x => x.length > 1)
      .map(x => x.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));

    if (escaped.length) {
      const re = new RegExp(`\\b(${escaped.join('|')})\\b`, 'gi');

      res = res.replace(re, (match) => {
        const lower = match.toLowerCase();

        let color = ANSI.yellow;
      if (/(error|fail|fault|critical|exception)/.test(lower)) color = ANSI.red;
      else if (/(success|ok|passed|done)/.test(lower)) color = ANSI.cyan;
      else if (/(warn|wait|pending)/.test(lower)) color = ANSI.yellow;
      else if (/(info|debug|trace)/.test(lower)) color = ANSI.blue;

      return `${color}${match}${ANSI.reset}`;
    });
  }

  if (semanticHlEnabled) {
    res = res.replace(/\b(error|failed|denied|fatal|invalid)\b/gi, `${ANSI.red}$1${ANSI.reset}`);
  }
  }

  return res;
}

export function updateKeywords(val) {
  activeKeywords.length = 0;

  if (val) {
    for (const x of val.split(/\s+/)) {
      if (x.length > 1) activeKeywords.push(x);
    }
  }

  window.rterm?.saveSetting('highlight_keywords', val);
}
