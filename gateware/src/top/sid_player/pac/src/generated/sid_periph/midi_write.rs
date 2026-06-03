#[doc = "Register `midi_write` reader"]
pub type R = crate::R<MIDI_WRITE_SPEC>;
#[doc = "Register `midi_write` writer"]
pub type W = crate::W<MIDI_WRITE_SPEC>;
#[doc = "Field `msg` writer - msg field"]
pub type MSG_W<'a, REG> = crate::FieldWriter<'a, REG, 24, u32>;
impl W {
    #[doc = "Bits 0:23 - msg field"]
    #[inline(always)]
    pub fn msg(&mut self) -> MSG_W<'_, MIDI_WRITE_SPEC> {
        MSG_W::new(self, 0)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`midi_write::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`midi_write::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct MIDI_WRITE_SPEC;
impl crate::RegisterSpec for MIDI_WRITE_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`midi_write::R`](R) reader structure"]
impl crate::Readable for MIDI_WRITE_SPEC {}
#[doc = "`write(|w| ..)` method takes [`midi_write::W`](W) writer structure"]
impl crate::Writable for MIDI_WRITE_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets midi_write to value 0"]
impl crate::Resettable for MIDI_WRITE_SPEC {}
