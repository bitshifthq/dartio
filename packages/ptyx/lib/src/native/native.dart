library;

import 'dart:async';
import 'dart:ffi';
import 'dart:io' show Directory, Platform, ProcessSignal;
import 'dart:isolate';
import 'dart:typed_data';

import '../api/api.dart';
import '../ffi/ptyx.g.dart';
import 'bindings.dart';
import 'errors.dart';
import 'event_decoder.dart';
import 'types.dart';

part 'runtime.dart';
part 'session.dart';

Future<PtySession> spawnSession(PtySpawnOptions options) =>
    _NativeSession.spawn(options);
