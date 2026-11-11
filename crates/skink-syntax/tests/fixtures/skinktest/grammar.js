// Skink test grammar (MIT OR Apache-2.0): words, numbers and `#` comments.
module.exports = grammar({
  name: 'skinktest',
  extras: $ => [/\s/, $.comment],
  rules: {
    source_file: $ => repeat(choice($.keyword, $.number, $.word)),
    keyword: _ => choice('let', 'fn'),
    number: _ => /\d+/,
    word: _ => /[a-z_]+/,
    comment: _ => token(seq('#', /.*/)),
  },
});
