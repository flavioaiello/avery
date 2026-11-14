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

function calendarKey(spec) {
  var m = /\.byId\("([^"\\]+)"\)$/.exec(Automation.getDisplayString(spec));
  if (!m) { throw new Error('calendar_id_unavailable'); }
  return m[1];
}

function listCalendars() {
  var all = calendarApp().calendars(), out = [];
  for (var i = 0; i < all.length; i++) {
    var writable = false;
    try { writable = !!all[i].writable(); } catch (e) {}
    out.push({id: calendarKey(all[i]), name: String(all[i].name()), writable: writable});
  }
  return out;
}

function calendarById(id) {
  var all = calendarApp().calendars();
  for (var i = 0; i < all.length; i++) {
    if (calendarKey(all[i]) === String(id)) { return all[i]; }
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

function seriesEnded(rule, from) {
  var m = /(?:^|;)UNTIL=(\d{4})(\d{2})(\d{2})/.exec(rule);
  return !!m && new Date(+m[1], +m[2] - 1, +m[3]).getTime() + 2 * 86400000 < from.getTime();
}

function listEvents(cal, req) {
  var from = localDate(req.from), to = localDate(req.to), evs = cal.events;
  var starts = evs.startDate();
  if (starts.length > req.maxScan) { throw new Error('calendar_too_large:' + starts.length); }
  var ends = evs.endDate(), recs = evs.recurrence(), picked = [], i, rule;
  for (i = 0; i < starts.length; i++) {
    if (starts[i] >= to) { continue; }
    rule = recurrenceOf(recs[i]);
    if (rule !== '' ? !seriesEnded(rule, from) : (ends[i] > from || (+starts[i] === +ends[i] && starts[i] >= from))) {
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
    if (Array.from(after.notes).length > (req.undo ? 4000 : 1200)) { throw new Error('notes_too_long'); }
  } else {
    ev = verifiedEvent(cal, req);
    if (req.action === 'update') { localDate(after.start); localDate(after.end); }
  }
  decision = changeDecision(req, before, after);
  if (decision === 'declined') { return {status: 'declined'}; }
  if (decision === 'timeout') { return {status: 'timeout'}; }
  cal = calendarById(req.calendarId);
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
