#[doc = "Register `midi_read` reader"]
pub type R = crate::R<MIDI_READ_SPEC>;
#[doc = "Register `midi_read` writer"]
pub type W = crate::W<MIDI_READ_SPEC>;
#[doc = "Field `msg` reader - msg field"]
pub type MSG_R = crate::FieldReader<u32>;
impl R {
    #[doc = "Bits 0:23 - msg field"]
    #[inline(always)]
    pub fn msg(&self) -> MSG_R {
        MSG_R::new(self.bits & 0x00ff_ffff)
    }
}
impl W {}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`midi_read::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`midi_read::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct MIDI_READ_SPEC;
impl crate::RegisterSpec for MIDI_READ_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`midi_read::R`](R) reader structure"]
impl crate::Readable for MIDI_READ_SPEC {}
#[doc = "`write(|w| ..)` method takes [`midi_read::W`](W) writer structure"]
impl crate::Writable for MIDI_READ_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets midi_read to value 0"]
impl crate::Resettable for MIDI_READ_SPEC {}
