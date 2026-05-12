import 'package:meta/meta.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart' as native;

@internal
PtyTermMode ptyTermModeFromNative(native.term_mode value) {
  final fields = value.valid_fields;
  return PtyTermMode(
    canonical: fields & native.PTYX_TERM_MODE_CANONICAL_VALID == 0
        ? null
        : value.canonical,
    echo: fields & native.PTYX_TERM_MODE_ECHO_VALID == 0 ? null : value.echo,
    signals: fields & native.PTYX_TERM_MODE_SIGNALS_VALID == 0
        ? null
        : value.signals,
  );
}

@internal
void setNativeSize(native.size target, PtySize source) {
  target.rows = source.rows;
  target.columns = source.columns;
  target.pixel_width = source.pixelWidth;
  target.pixel_height = source.pixelHeight;
}

@internal
PtySize sizeFromNative(native.size source) => PtySize(
  rows: source.rows,
  columns: source.columns,
  pixelWidth: source.pixel_width,
  pixelHeight: source.pixel_height,
);

@internal
extension PtyEnvironmentModeNative on PtyEnvironmentMode {
  int get nativeValue => switch (this) {
    .inherit => native.PTYX_ENV_INHERIT,
    .overlay => native.PTYX_ENV_OVERLAY,
    .replace => native.PTYX_ENV_REPLACE,
    .clear => native.PTYX_ENV_CLEAR,
  };
}
