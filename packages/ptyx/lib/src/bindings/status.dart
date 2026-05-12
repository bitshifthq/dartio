import 'dart:ffi';

import 'package:ffi/ffi.dart';
import 'package:meta/meta.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart' as native;

@internal
void checkStatus(int status) {
  if (isOkStatus(status)) return;
  throw exceptionFromStatus(status);
}

@internal
PtyException exceptionFromParts(int status, String message) {
  if (status == native.PTYX_STATUS_UNSUPPORTED) {
    return PtyUnsupportedException(message);
  }
  if (status == native.PTYX_STATUS_CLOSED) {
    return PtyClosedException(message);
  }
  return PtyException(message);
}

@internal
PtyException exceptionFromStatus(int status) {
  return exceptionFromParts(status, nativeErrorMessage(status));
}

@internal
bool isOkStatus(int status) => status == native.PTYX_STATUS_OK;

@internal
bool isUnsupportedStatus(int status) {
  return status == native.PTYX_STATUS_UNSUPPORTED;
}

@internal
String? lastNativeErrorMessage() {
  final pointer = native.ptyx_last_error_message();
  if (pointer == nullptr) return null;
  return pointer.cast<Utf8>().toDartString();
}

@internal
String nativeErrorMessage(int status) {
  return lastNativeErrorMessage() ?? nativeStatusString(status);
}

@internal
String nativeStatusString(int status) {
  return native.ptyx_status_string(status).cast<Utf8>().toDartString();
}
