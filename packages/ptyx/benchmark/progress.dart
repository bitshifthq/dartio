import 'dart:convert';
import 'dart:io';

/// Persists diagnostic checkpoints independently from a final result.
///
/// Each checkpoint is a complete JSON line. A process that terminates before
/// producing its final artifact therefore leaves its last completed phase.
final class DiagnosticProgressReporter {
  /// Creates a reporter that appends to [path] and emits each line through
  /// [log].
  DiagnosticProgressReporter({
    required this.path,
    Future<void> Function(String line)? log,
  }) : _log = log ?? _writeStderr;

  /// The JSON Lines file to update, or `null` when only logging is required.
  final String? path;

  final Future<void> Function(String line) _log;

  /// Flushes [phase] and its optional [details] to every configured sink.
  Future<void> record(
    String phase, {
    Map<String, Object?> details = const {},
  }) async {
    final line = jsonEncode({
      'schema': 1,
      'timestamp_utc': DateTime.now().toUtc().toIso8601String(),
      'pid': pid,
      'phase': phase,
      if (details.isNotEmpty) 'details': details,
    });
    if (path != null) {
      await File(path!).writeAsString('$line\n', mode: .append, flush: true);
    }
    await _log(line);
  }

  static Future<void> _writeStderr(String line) async {
    stderr.writeln('[ptyx-progress] $line');
    await stderr.flush();
  }
}
