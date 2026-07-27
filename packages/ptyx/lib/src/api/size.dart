part of 'api.dart';

/// The cell and pixel dimensions of a pseudo terminal.
///
/// [rows] and [columns] describe the terminal grid seen by terminal programs.
/// [pixelWidth] and [pixelHeight] describe the rendered terminal area when it
/// is known. Use zero for unknown pixel dimensions.
///
/// Example:
///
/// ```dart
/// const size = PtySize(rows: 24, columns: 80);
/// session.resize(size);
/// ```
@immutable
final class PtySize {
  /// Terminal rows.
  ///
  /// Must be greater than zero when passed to [PtySession.spawn] or
  /// [PtySession.resize].
  final int rows;

  /// Terminal columns.
  ///
  /// Must be greater than zero when passed to [PtySession.spawn] or
  /// [PtySession.resize].
  final int columns;

  /// Terminal width in pixels.
  ///
  /// Use zero when the width is unknown.
  final int pixelWidth;

  /// Terminal height in pixels.
  ///
  /// Use zero when the height is unknown.
  final int pixelHeight;

  /// Creates a terminal size.
  const PtySize({
    required this.rows,
    required this.columns,
    this.pixelWidth = 0,
    this.pixelHeight = 0,
  });

  @override
  bool operator ==(Object other) =>
      other is PtySize &&
      rows == other.rows &&
      columns == other.columns &&
      pixelWidth == other.pixelWidth &&
      pixelHeight == other.pixelHeight;

  @override
  int get hashCode => Object.hash(rows, columns, pixelWidth, pixelHeight);
}
