import 'package:ptyx/ptyx.dart'
    show PtyClosedException, PtyException, PtyUnsupportedException;
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
          'PtyException: base',
          'PtyClosedException: closed',
          'PtyUnsupportedException: unsupported',
        ]);
      });
    });
  });
}
