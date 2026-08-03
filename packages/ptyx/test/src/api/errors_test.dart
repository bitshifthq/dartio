import 'package:ptyx/ptyx.dart'
    show
        PtyClosedException,
        PtyErrorCategory,
        PtyException,
        PtyUnsupportedException;
import 'package:test/test.dart';

void main() {
  group('PtyException', () {
    group('toString', () {
      test('formats messages with the concrete exception type', () {
        final messages = [
          const PtyException('base').toString(),
          const PtyClosedException('closed').toString(),
          const PtyUnsupportedException('unsupported').toString(),
        ];

        expect(messages, [
          'PtyException[unknown/unknown]: base',
          'PtyClosedException[state/closed]: closed',
          'PtyUnsupportedException[capability/unsupported]: unsupported',
        ]);
      });

      test('retains operation, category, native code, and safe context', () {
        const error = PtyException(
          'delivery failed',
          operation: 'kill',
          category: .process,
          nativeCode: 3,
          context: 'foreground process group',
        );

        expect(error.operation, 'kill');
        expect(error.category, PtyErrorCategory.process);
        expect(error.nativeCode, 3);
        expect(error.context, 'foreground process group');
        expect(
          error.toString(),
          'PtyException[kill/process] (native 3): delivery failed: '
          'foreground process group',
        );
      });
    });
  });
}
