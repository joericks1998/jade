(function () {
  var style = document.createElement('style');
  style.textContent = [
    'pre { position: relative; }',
    '.copy-btn {',
    '  position: absolute;',
    '  top: 0.5rem;',
    '  right: 0.5rem;',
    '  background: rgba(30,122,46,0.08);',
    '  border: 1px solid rgba(30,122,46,0.22);',
    '  color: #2e5c2e;',
    '  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;',
    '  font-size: 0.7rem;',
    '  line-height: 1;',
    '  padding: 0.28rem 0.55rem;',
    '  border-radius: 4px;',
    '  cursor: pointer;',
    '  opacity: 0;',
    '  transition: opacity 0.15s, background 0.15s;',
    '  user-select: none;',
    '}',
    'pre:hover .copy-btn { opacity: 1; }',
    '.copy-btn:hover { background: rgba(30,122,46,0.18); }',
    '.copy-btn.copied {',
    '  color: #1e7a2e;',
    '  border-color: #1e7a2e;',
    '  background: rgba(30,122,46,0.15);',
    '  opacity: 1;',
    '}',
  ].join('\n');
  document.head.appendChild(style);

  function addButton(pre) {
    var btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.setAttribute('aria-label', 'Copy code');
    btn.textContent = 'Copy';

    btn.addEventListener('click', function () {
      var target = pre.querySelector('code') || pre;
      var text = target.innerText;

      function markCopied() {
        btn.textContent = 'Copied!';
        btn.classList.add('copied');
        setTimeout(function () {
          btn.textContent = 'Copy';
          btn.classList.remove('copied');
        }, 2000);
      }

      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(markCopied).catch(fallback);
      } else {
        fallback();
      }

      function fallback() {
        var ta = document.createElement('textarea');
        ta.value = text;
        ta.style.cssText = 'position:fixed;opacity:0;pointer-events:none';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        try { document.execCommand('copy'); } catch (_) {}
        document.body.removeChild(ta);
        markCopied();
      }
    });

    pre.appendChild(btn);
  }

  document.querySelectorAll('pre').forEach(addButton);
})();
