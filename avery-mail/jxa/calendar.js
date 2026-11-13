function run(argv) {
  var req = JSON.parse(argv[0]);
  try {
    return JSON.stringify({ok: true, data: dispatch(req)});
  } catch (e) {
    return JSON.stringify({ok: false, error: String((e && e.message) || e)});
  }
}

function calendarApp() { return Application('Calendar'); }

function dispatch(req) {
  if (req.op === 'calendars') { return listCalendars(); }
  var cal = calendarById(req.calendarId);
  if (req.op === 'events') { return listEvents(cal, req); }
  if (req.op === 'event') { return eventDetail(cal, req); }
  if (req.op === 'change') { return changeEvent(cal, req); }
  throw new Error('unknown_op');
}

function listCalendars() {
  var all = calendarApp().calendars(), out = [];
  for (var i = 0; i < all.length; i++) {
    var writable = false;
    try { writable = !!all[i].writable(); } catch (e) {}
    out.push({id: String(all[i].calendarIdentifier()), name: String(all[i].name()), writable: writable});
  }
  return out;
}

function calendarById(id) {
  var app = calendarApp(), ids = app.calendars.calendarIdentifier();
  for (var i = 0; i < ids.length; i++) {
    if (String(ids[i]) === String(id)) { return app.calendars[i]; }
  }
  throw new Error('calendar_not_found');
}

function localDate(text) {
  var m = /^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2})$/.exec(String(text));
  if (!m) { throw new Error('bad_time'); }
  var d = new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], 0, 0);
  if (d.getFullYear() !== +m[1] || d.getMonth() !== +m[2] - 1 || d.getDate() !== +m[3] ||
      d.getHours() !== +m[4] || d.getMinutes() !== +m[5]) {
    throw new Error('invalid_local_time');
  }
  return d;
}

function textOf(value) {
  return (value === null || value === undefined || String(value) === 'missing value') ? '' : String(value);
}

function recurrenceOf(value) { return textOf(value); }

function excludedOf(ev) {
  var out = [];
  try {
    var list = ev.excludedDates();
    for (var i = 0; list && i < list.length && i < 400; i++) { out.push(fmt(list[i])); }
  } catch (e) {}
  return out;
}

function attendeeCount(ev) {
  try { return ev.attendees().length; } catch (e) { return -1; }
}

function listEvents(cal, req) {
  var from = localDate(req.from), to = localDate(req.to), evs = cal.events;
  var starts = evs.startDate();
  if (starts.length > req.maxScan) { throw new Error('too_many_events:' + starts.length); }
  var ends = evs.endDate(), recs = evs.recurrence(), picked = [], i;
  for (i = 0; i < starts.length; i++) {
    if (starts[i] >= to) { continue; }
    if (recurrenceOf(recs[i]) !== '' || ends[i] > from ||
        (+starts[i] === +ends[i] && starts[i] >= from)) {
      picked.push(i);
    }
  }
  if (picked.length > req.maxItems) { throw new Error('too_many_events:' + picked.length); }
  if (!picked.length) { return []; }
  var uids = evs.uid(), subjects = evs.summary(), places = evs.location(), flags = evs.alldayEvent();
  var out = [], index, recurrence;
  for (i = 0; i < picked.length; i++) {
    index = picked[i];
    recurrence = recurrenceOf(recs[index]);
    out.push({
      uid: textOf(uids[index]), summary: textOf(subjects[index]),
      start: fmt(starts[index]), end: fmt(ends[index]), allDay: flags[index] === true,
      location: textOf(places[index]), recurrence: recurrence,
      excluded: recurrence === '' ? [] : excludedOf(evs[index]),
      utcOffset: -starts[index].getTimezoneOffset(), attendees: attendeeCount(evs[index])
    });
  }
  return out;
}

function findEvent(cal, uid) {
  var sel = cal.events.whose({uid: String(uid)}), ids = sel.uid();
  if (ids.length === 0) { throw new Error('event_not_found'); }
  if (ids.length > 1) { throw new Error('ambiguous_event'); }
  return sel[0];
}

function people(ev) {
  var out = [];
  try {
    var list = ev.attendees();
    for (var i = 0; i < list.length && i < 50; i++) {
      out.push({name: textOf(list[i].displayName()), email: textOf(list[i].email()),
                status: textOf(list[i].participationStatus())});
    }
  } catch (e) {}
  return out;
}

function eventDetail(cal, req) {
  var ev = findEvent(cal, req.uid);
  var start = ev.startDate(), end = ev.endDate(), recurrence = recurrenceOf(ev.recurrence());
  return {
    uid: textOf(ev.uid()), summary: textOf(ev.summary()), start: fmt(start), end: fmt(end),
    allDay: ev.alldayEvent() === true, location: textOf(ev.location()), recurrence: recurrence,
    excluded: recurrence === '' ? [] : excludedOf(ev), utcOffset: -start.getTimezoneOffset(),
    attendees: attendeeCount(ev), notes: textOf(ev.description()), url: textOf(ev.url()),
    people: people(ev)
  };
}

function fieldsOf(ev) {
  return {summary: textOf(ev.summary()), start: fmt(ev.startDate()), end: fmt(ev.endDate()),
          allDay: ev.alldayEvent() === true, location: textOf(ev.location()),
          notes: textOf(ev.description()), url: textOf(ev.url())};
}

function sameFields(a, b) {
  return !!a && !!b && a.summary === b.summary && a.start === b.start && a.end === b.end &&
         a.allDay === b.allDay && a.location === b.location && a.notes === b.notes && a.url === b.url;
}

function guardEvent(ev) {
  if (recurrenceOf(ev.recurrence()) !== '') { throw new Error('recurring_event'); }
  if (attendeeCount(ev) !== 0) { throw new Error('has_attendees'); }
}

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

function noteBlock(label, text, cap) {
  var body = shown(text).replace(/\r\n?/g, '\n').trim();
  var chars = Array.from(body);
  if (chars.length > cap) { body = chars.slice(0, cap).join('') + '\n[\u2026 ' + (chars.length - cap) + ' weitere Zeichen]'; }
  return [label, '--------------------------------', body === '' ? '(leer)' : body, '--------------------------------'];
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
    if (after.notes !== '') { lines = lines.concat(noteBlock('Notizen:', after.notes, 1200)); }
  } else if (req.action === 'delete') {
    lines.push(row('Titel', before.summary), row('Wann', whenText(before)));
    if (before.location !== '') { lines.push(row('Ort', before.location)); }
    if (before.notes !== '') { lines = lines.concat(noteBlock('Notizen:', before.notes, 600)); }
  } else {
    lines.push(changedRow('Titel', before.summary, after.summary), changedRow('Wann', whenText(before), whenText(after)));
    if (before.location !== '' || after.location !== '') { lines.push(changedRow('Ort', before.location, after.location)); }
    if (before.notes !== after.notes) {
      lines = lines.concat(noteBlock('Notizen vorher:', before.notes, 600), noteBlock('Notizen nachher:', after.notes, 1200));
    }
  }
  return lines;
}

function applyUpdate(ev, before, after) {
  if (before.summary !== after.summary) { ev.summary = after.summary; }
  if (before.location !== after.location) { ev.location = after.location; }
  if (before.notes !== after.notes) { ev.description = after.notes; }
  if (before.start !== after.start || before.end !== after.end) {
    var newStart = localDate(after.start), newEnd = localDate(after.end), oldEnd = localDate(before.end);
    if (newStart >= oldEnd) { ev.endDate = newEnd; ev.startDate = newStart; }
    else { ev.startDate = newStart; ev.endDate = newEnd; }
  }
}

function verifiedEvent(cal, req) {
  var ev = findEvent(cal, req.uid);
  guardEvent(ev);
  if (!sameFields(fieldsOf(ev), req.before)) { throw new Error('event_changed'); }
  return ev;
}

function changeEvent(cal, req) {
  var app = calendarApp(), before = req.before, after = req.after, ev, decision, uid = req.uid;
  if (!cal.writable()) { throw new Error('calendar_read_only'); }
  if (req.action === 'create') {
    localDate(after.start); localDate(after.end);
    if (Array.from(after.notes).length > 1200) { throw new Error('notes_too_long'); }
  } else {
    ev = verifiedEvent(cal, req);
    if (req.action === 'update') { localDate(after.start); localDate(after.end); }
  }
  decision = showDialog(req.undo ? 'Goose: \u00c4nderung r\u00fcckg\u00e4ngig machen' : 'Goose m\u00f6chte einen Termin \u00e4ndern',
                        dialogLines(req, before, after),
                        {create: 'Anlegen', update: '\u00c4ndern', 'delete': 'L\u00f6schen'}[req.action], req.seconds);
  if (decision === 'declined') { return {status: 'declined'}; }
  if (decision === 'timeout') { return {status: 'timeout'}; }
  if (!cal.writable()) { throw new Error('calendar_read_only'); }
  if (req.action !== 'create') { ev = verifiedEvent(cal, req); }
  var actual = null;
  try {
    if (req.action === 'create') {
      var props = {summary: after.summary, startDate: localDate(after.start), endDate: localDate(after.end),
                   alldayEvent: after.allDay, location: after.location, description: after.notes};
      if (after.url !== '') { props.url = after.url; }
      ev = app.Event(props);
      cal.events.push(ev);
      uid = textOf(ev.uid());
      actual = fieldsOf(ev);
    } else if (req.action === 'update') {
      applyUpdate(ev, before, after);
      actual = fieldsOf(ev);
    } else {
      app['delete'](ev);
      if (cal.events.whose({uid: String(uid)}).uid().length > 0) { throw new Error('event still present'); }
    }
  } catch (e) {
    throw new Error('change_state_unknown: ' + String((e && e.message) || e));
  }
  return {status: 'applied', uid: uid, after: actual};
}
