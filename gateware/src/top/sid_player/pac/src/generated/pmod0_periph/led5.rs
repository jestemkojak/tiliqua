#[doc = "Register `led5` reader"]
pub type R = crate::R<LED5_SPEC>;
#[doc = "Register `led5` writer"]
pub type W = crate::W<LED5_SPEC>;
#[doc = "Field `led` writer - led field"]
pub type LED_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
impl W {
    #[doc = "Bits 0:7 - led field"]
    #[inline(always)]
    pub fn led(&mut self) -> LED_W<'_, LED5_SPEC> {
        LED_W::new(self, 0)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`led5::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`led5::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct LED5_SPEC;
impl crate::RegisterSpec for LED5_SPEC {
    type Ux = u8;
}
#[doc = "`read()` method returns [`led5::R`](R) reader structure"]
impl crate::Readable for LED5_SPEC {}
#[doc = "`write(|w| ..)` method takes [`led5::W`](W) writer structure"]
impl crate::Writable for LED5_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets led5 to value 0"]
impl crate::Resettable for LED5_SPEC {}
