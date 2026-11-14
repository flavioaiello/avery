function lastDay(f) {
  var s = f.start.slice(0, 10), e = f.end.slice(0, 10);
  if (f.end.slice(11) === '00:00' && e > s) {
    return fmt(new Date(+e.slice(0, 4), +e.slice(5, 7) - 1, +e.slice(8, 10) - 1)).slice(0, 10);
  }
  return e < s ? s : e;
}

function whenText(f) {
  if (f.allDay) {
    var last = lastDay(f);
    return f.start.slice(0, 10) + (last > f.start.slice(0, 10) ? ' bis ' + last : '') + ' (ganzt\u00e4gig)';
  }
  return f.start + ' bis ' + f.end;
}

var NOTE_MARK = '\u2502 \u200e', NOTE_RULE = '--------------------------------';
var AFTER_LIMIT = [30, 1200], BEFORE_LIMIT = [8, 300], BODY_LIMIT = [60, 3200];

function noteRows(text) {
  var lines = shown(text).replace(/\r\n?/g, '\n').replace(/\t/g, ' ').trim().split('\n'), rows = [], line, i;
  for (i = 0; i < lines.length; i++) {
    line = lines[i].replace(/\s+$/, '');
    if (line !== '' || rows.length === 0 || rows[rows.length - 1] !== '') { rows.push(line); }
  }
  return rows;
}

function markedLines(text, limit) {
  var rows = noteRows(text), out = [], left = limit[1], i = 0, rest = 0, chars;
  if (rows[0] === '') { return ['(leer)']; }
  while (i < rows.length && out.length < limit[0] && left > 0) {
    chars = Array.from(rows[i++]);
    if (chars.length > left) { rest = chars.length - left; chars = chars.slice(0, left); }
    out.push(NOTE_MARK + chars.join('') + (rest > 0 ? '\u2026' : ''));
    left -= chars.length + 1;
    if (rest > 0) { break; }
  }
  if (i < rows.length) { out.push('[\u2026 ' + (rows.length - i) + ' weitere Zeilen]'); }
  else if (rest > 0) { out.push('[\u2026 ' + rest + ' weitere Zeichen]'); }
  return out;
}

function noteBlock(label, text, limit) {
  return [label, NOTE_RULE].concat(markedLines(text, limit), [NOTE_RULE]);
}

function rowOf(label, text) { return (label + ':          ').slice(0, 12) + text; }

function row(label, text) { return rowOf(label, flat(text, 300)); }

function changedRow(label, oldText, newText) {
  return oldText === newText ? rowOf(label, flat(oldText, 300) + '  (unver\u00e4ndert)')
                             : rowOf(label, flat(oldText, 300) + '  \u2192  ' + flat(newText, 300));
}

function dialogLines(req, before, after) {
  var verbs = {create: 'ANLEGEN', update: '\u00c4NDERN', 'delete': 'L\u00d6SCHEN'};
  var head = req.undo ? 'Goose m\u00f6chte eine \u00c4nderung R\u00dcCKG\u00c4NGIG machen: Termin ' : 'Goose m\u00f6chte einen Termin ';
  var lines = [head + verbs[req.action] + '.',
               'Pr\u00fcfe alle Angaben. Nur best\u00e4tigen, wenn du das gerade selbst veranlasst hast.', '',
               row('Kalender', req.calendarLabel)];
  if (req.action === 'create') {
    lines.push(row('Titel', after.summary), row('Wann', whenText(after)));
    if (after.location !== '') { lines.push(row('Ort', after.location)); }
    if (after.notes !== '') { lines = lines.concat(noteBlock('Notizen:', after.notes, AFTER_LIMIT)); }
  } else if (req.action === 'delete') {
    lines.push(row('Titel', before.summary), row('Wann', whenText(before)));
    if (before.location !== '') { lines.push(row('Ort', before.location)); }
    if (before.notes !== '') { lines = lines.concat(noteBlock('Notizen:', before.notes, BEFORE_LIMIT)); }
  } else {
    lines.push(changedRow('Titel', before.summary, after.summary), changedRow('Wann', whenText(before), whenText(after)));
    if (before.location !== '' || after.location !== '') { lines.push(changedRow('Ort', before.location, after.location)); }
    if (before.notes !== after.notes) {
      lines = lines.concat(noteBlock('Notizen nachher:', after.notes, AFTER_LIMIT),
                           noteBlock('Notizen vorher:', before.notes, BEFORE_LIMIT));
    }
  }
  return lines;
}

function flatList(list) {
  var out = [];
  for (var i = 0; i < list.length; i++) { out.push(flat(list[i], 300)); }
  return out.join(', ');
}

function askApproval(p, seconds) {
  var label = p.sendLabel || 'Senden';
  var lines = [p.note || 'Pr\u00fcfe Empf\u00e4nger und Text. Nur senden, wenn du das gerade selbst veranlasst hast.', ''];
  lines.push('Von:        ' + flat(p.from, 200));
  lines.push('An:         ' + flatList(p.to));
  if (p.cc.length) { lines.push('Cc:         ' + flatList(p.cc)); }
  lines.push('Betreff:    ' + flat(p.subject, 300));
  if (p.reply) { lines.push('Antwort auf: ' + flat(p.reply.subject, 150) + ' (von ' + flat(p.reply.from, 100) + ')'); }
  lines.push('', NOTE_RULE);
  lines = lines.concat(markedLines(p.body, BODY_LIMIT), [NOTE_RULE]);
  var decision = showDialog(p.title || 'Goose m\u00f6chte eine E-Mail senden', lines, label, seconds);
  return decision === 'approved' ? 'send' : decision;
}

function changeDecision(req, before, after) {
  return showDialog(req.undo ? 'Goose: \u00c4nderung r\u00fcckg\u00e4ngig machen' :
                    {create: 'Goose m\u00f6chte einen Termin anlegen', update: 'Goose m\u00f6chte einen Termin \u00e4ndern',
                     'delete': 'Goose m\u00f6chte einen Termin l\u00f6schen'}[req.action],
                    dialogLines(req, before, after),
                    {create: 'Anlegen', update: '\u00c4ndern', 'delete': 'L\u00f6schen'}[req.action], req.seconds);
}
