# Sobres 1.0.5: release notes (draft, 2026-11-24)

Each line is a change committed after v1.0.4 (80d3004): c9adf5f, 94f37c6, b5a220f, ad045d0, 09d34b2, 9812d57,
253cef8, a5bce05, and the 2026-11-24 safety audit fixes (research/safety-audit-2026-11-24/sobres.md S4-1..S4-4).

## Play "What's new" (500 characters max each)

en-US:
Turning the phone or switching dark mode keeps you where you were. Colour choices are bigger and named for screen readers. "Removed 1 entry" reads right. A backup that can't be saved leaves no empty file behind, and never removes an older backup you chose to replace. "Undo someone's changes" now really puts back what that person changed in a budget (before, it found nothing to undo).

es-419:
Girar el teléfono o cambiar al modo oscuro te deja donde estabas. Los colores para elegir son más grandes y tienen nombre para los lectores de pantalla. "Se quitó 1 movimiento" se lee bien. Una copia que no se pudo guardar no deja un archivo vacío, y nunca borra una copia anterior que elegiste reemplazar. "Deshacer cambios de alguien" ahora sí devuelve lo que esa persona cambió en un presupuesto (antes no encontraba nada para deshacer).

## GitHub release (Windows extras on top of the above)

- Saving a backup file says whether it worked, and replacing an older backup leaves no hidden ".prev" copy next to it.
- Feedback names your system by its release (e.g. "Windows 11"), not a build string.
