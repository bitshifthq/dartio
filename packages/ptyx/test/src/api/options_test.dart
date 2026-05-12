import 'package:ptyx/ptyx.dart'
    show PtyEnvironmentMode, PtySize, PtySpawnOptions;
import 'package:test/test.dart';

void main() {
  group('PtySpawnOptions', () {
    group('constructor', () {
      test('uses overlay environment defaults', () {
        const options = PtySpawnOptions(
          executable: '/bin/sh',
          initialSize: PtySize(rows: 24, columns: 80),
        );

        final defaults = (
          options.arguments.length,
          options.environment.length,
          options.environmentMode,
          options.workingDirectory,
        );

        expect(defaults, (0, 0, PtyEnvironmentMode.overlay, null));
      });
    });
  });
}
