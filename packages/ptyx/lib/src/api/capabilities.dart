part of 'api.dart';

/// Native features available for a [PtySession].
@immutable
final class PtyCapabilities {
  /// Whether signals can be delivered with Unix signal semantics.
  final bool signals;

  /// Whether the session owns a Unix terminal process group.
  final bool processGroups;

  /// Whether terminal-mode snapshots are available.
  final bool terminalModes;

  /// Whether the backend uses Windows ConPTY.
  final bool conPty;

  /// Creates an immutable capability snapshot.
  const PtyCapabilities({
    required this.signals,
    required this.processGroups,
    required this.terminalModes,
    required this.conPty,
  });
}
