import 'dart:ffi';

import 'package:meta/meta.dart';

import '../api/api.dart';
import '../ffi/ptyx.g.dart' as native;
import 'status.dart';

var _dartInitialized = false;

@internal
void ensureDartInitialized() {
  if (_dartInitialized) return;
  _checkNativeAbi();
  checkStatus(native.ptyx_init(NativeApi.initializeApiDLData));
  _dartInitialized = true;
}

void _checkNativeAbi() {
  final loadedMajor = native.ptyx_abi_version_major();
  if (loadedMajor == native.PTYX_ABI_VERSION_MAJOR) return;
  throw PtyException(
    'ptyx native ABI mismatch: loaded v$loadedMajor, '
    'expected v${native.PTYX_ABI_VERSION_MAJOR}. Rebuild native assets.',
  );
}
