# Blueprint files are read as C (xgettext warns about the extension, which
# is harmless): their _("…") and C_("…", "…") calls look the same.
i18n.gettext(
  'lia-files',
  preset: 'glib',
  args: [
    '--keyword=gettext',
    '--keyword=ngettext:1,2',
    '--keyword=pgettext:1c,2',
    '--keyword=npgettext:1c,2,3',
    '--add-comments=Translators',
  ],
)
