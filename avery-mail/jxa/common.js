function pad(n) { return (n < 10 ? '0' : '') + n; }
function fmt(d) {
  return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) +
    ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
}

function sleepSeconds(n) {
  try {
    var sa = Application.currentApplication();
    sa.includeStandardAdditions = true;
    sa.delay(n);
  } catch (e) {}
}

function hidden(cp) {
  for (var k = 0; k < INVISIBLE_RANGES.length; k++) {
    if (cp >= INVISIBLE_RANGES[k][0] && cp <= INVISIBLE_RANGES[k][1]) { return true; }
  }
  return false;
}

function shown(value) {
  var s = String(value === null || value === undefined ? '' : value), out = '', i, cp;
  for (i = 0; i < s.length; i++) {
    cp = s.codePointAt(i);
    if (cp > 0xffff) { i++; }
    if (!hidden(cp)) { out += String.fromCodePoint(cp); }
  }
  return out;
}

function flat(value, max) {
  var chars = Array.from(shown(String(value === null || value === undefined ? '' : value).replace(/[\s\u0085]+/g, ' ')).trim());
  return chars.length > max ? chars.slice(0, max - 1).join('') + '\u2026' : chars.join('');
}

function showDialog(title, lines, label, seconds) {
  var app = Application.currentApplication();
  app.includeStandardAdditions = true;
  try { app.activate(); } catch (e) {}
  var answer;
  try {
    answer = app.displayDialog(lines.join('\n'), {
      withTitle: title,
      buttons: ['Abbrechen', label],
      defaultButton: 'Abbrechen',
      cancelButton: 'Abbrechen',
      withIcon: 'caution',
      givingUpAfter: seconds
    });
  } catch (e) {
    var text = String((e && e.message) || e);
    if (text.indexOf('-128') >= 0) { return 'declined'; }
    throw new Error('approval_dialog_failed: ' + text);
  }
  if (answer.gaveUp) { return 'timeout'; }
  return answer.buttonReturned === label ? 'approved' : 'declined';
}
