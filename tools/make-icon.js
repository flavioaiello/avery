// Draws the Avery app icon (1024x1024 PNG): a rounded square with a gradient and a white "a".
// Usage: osascript -l JavaScript tools/make-icon.js OUTPUT.png
ObjC.import('AppKit');

function run(argv) {
  var out = argv[0];
  var size = 1024;
  var image = $.NSImage.alloc.initWithSize($.NSMakeSize(size, size));
  image.lockFocus;

  var margin = 100;
  var rect = $.NSMakeRect(margin, margin, size - 2 * margin, size - 2 * margin);
  var radius = 185;

  // soft shadow under the tile
  var shadow = $.NSShadow.alloc.init;
  shadow.setShadowOffset($.NSMakeSize(0, -14));
  shadow.setShadowBlurRadius(28);
  shadow.setShadowColor($.NSColor.colorWithSRGBRedGreenBlueAlpha(0, 0, 0, 0.35));
  $.NSGraphicsContext.currentContext.saveGraphicsState;
  shadow.set;
  var tile = $.NSBezierPath.bezierPathWithRoundedRectXRadiusYRadius(rect, radius, radius);
  $.NSColor.colorWithSRGBRedGreenBlueAlpha(0.2, 0.25, 0.8, 1).setFill;
  tile.fill;
  $.NSGraphicsContext.currentContext.restoreGraphicsState;

  var gradient = $.NSGradient.alloc.initWithStartingColorEndingColor(
    $.NSColor.colorWithSRGBRedGreenBlueAlpha(0.13, 0.55, 0.95, 1),
    $.NSColor.colorWithSRGBRedGreenBlueAlpha(0.48, 0.24, 0.80, 1));
  gradient.drawInBezierPathAngle(tile, -55);

  // thin highlight along the top edge
  var highlight = $.NSBezierPath.bezierPathWithRoundedRectXRadiusYRadius(
    $.NSInsetRect(rect, 3, 3), radius - 3, radius - 3);
  highlight.setLineWidth(5);
  $.NSColor.colorWithSRGBRedGreenBlueAlpha(1, 1, 1, 0.18).setStroke;
  highlight.stroke;

  // the letter
  var font = $.NSFont.systemFontOfSizeWeight(560, $.NSFontWeightBold);
  var attrs = $.NSMutableDictionary.alloc.init;
  attrs.setObjectForKey(font, $.NSFontAttributeName);
  attrs.setObjectForKey($.NSColor.whiteColor, $.NSForegroundColorAttributeName);
  var letter = $('a');
  var box = letter.sizeWithAttributes(attrs);
  letter.drawAtPointWithAttributes(
    $.NSMakePoint((size - box.width) / 2, (size - box.height) / 2 - 8), attrs);

  image.unlockFocus;
  var bitmap = $.NSBitmapImageRep.alloc.initWithData(image.TIFFRepresentation);
  var png = bitmap.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $());
  png.writeToFileAtomically(out, true);
  return out;
}
